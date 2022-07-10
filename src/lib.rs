use reqwest::{header, Client};
use serde::{Deserialize, Serialize};

#[derive(Debug)]
pub enum Error {
    Reqwest(reqwest::Error),
    Serde(serde_json::Error),
    Io(std::io::Error),
    Tokio(tokio::task::JoinError),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Reqwest(e) => write!(f, "Reqwest error: {}", e),
            Error::Serde(e) => write!(f, "Serde error: {}", e),
            Error::Io(e) => write!(f, "IO error: {}", e),
            Error::Tokio(e) => write!(f, "Join error: {}", e),
        }
    }
}
impl std::error::Error for Error {}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        Error::Reqwest(e)
    }
}
impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Serde(e)
    }
}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}
impl From<tokio::task::JoinError> for Error {
    fn from(e: tokio::task::JoinError) -> Self {
        Error::Tokio(e)
    }
}

type Result<T> = std::result::Result<T, Error>;

/// A struct that holds the data for a single file.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct FileId {
    pub(crate) asset_url: String,
    pub(crate) chunks: usize,
    pub(crate) file_name: String,
}

impl FileId {
    /// Uploads a file to the GitHub repository's releases.
    ///
    /// The token must have write access to the repository.
    /// `repo` must be in the format `owner/repo`.
    pub async fn upload_file(
        file_name: impl Into<String>,
        file_data: impl AsRef<[u8]>,
        repo: impl AsRef<str>,
        token: impl AsRef<str>,
    ) -> Result<Self> {
        let file_data = file_data.as_ref();

        let client = Client::builder()
            .default_headers({
                let mut map = header::HeaderMap::new();
                map.insert(header::AUTHORIZATION, {
                    let mut header =
                        header::HeaderValue::from_str(&format!("token {}", token.as_ref()))
                            .unwrap();
                    header.set_sensitive(true);
                    header
                });
                map
            })
            .build()?;

        let upload_url = create_or_get_release(repo.as_ref(), "files", client.clone()).await?;
        let url = upload_url
            .strip_suffix("{?name,label}")
            .unwrap_or(&upload_url)
            .parse::<url::Url>()
            .unwrap();

        let hash = sha256::digest_bytes(file_data);

        // split the file into chunks of ~100 megabytes
        // I know GitHub allows releases of 2 gigabytes, but it takes too long to upload that much data.
        let chunks = file_data.chunks(100_000_000);
        let chunks_len = chunks.len();

        let mut threads = Vec::with_capacity(chunks.len());

        for (i, chunk) in chunks.enumerate() {
            let mut url = url.clone();
            url.set_query(Some(&format!("name={hash}-chunk{i}")));

            let client = client.clone();
            let chunk = chunk.to_vec();
            threads.push(tokio::spawn(async move {
                client
                    .post(url)
                    .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
                    .header(reqwest::header::CONTENT_LENGTH, chunk.len())
                    .body(chunk)
                    .send()
                    .await
            }));
        }

        let mut asset_url = String::new();
        for (chunk, thread) in threads.into_iter().enumerate() {
            let ret = thread.await??;
            if chunk == 0 {
                let json = ret.json::<serde_json::Value>().await?;
                asset_url = json["url"].as_str().unwrap().to_string();
            }
        }

        Ok(Self {
            asset_url,
            chunks: chunks_len,
            file_name: file_name.into(),
        })
    }

    /// Downloads the file from the GitHub repository's releases.
    ///
    /// The token must have read access to the repository.
    pub async fn get_file<T: Into<String>>(self, token: Option<T>) -> Result<Vec<u8>> {
        let mut file = Vec::new();
        let mut threads = Vec::with_capacity(self.chunks);

        let client = if let Some(token) = token {
            let client = Client::builder()
                .default_headers({
                    let mut map = header::HeaderMap::new();
                    map.insert(header::AUTHORIZATION, {
                        let mut header =
                            header::HeaderValue::from_str(&format!("token {}", token.into()))
                                .unwrap();
                        header.set_sensitive(true);
                        header
                    });
                    map
                })
                .build()?;
            client
        } else {
            Client::new()
        };

        for i in 0..self.chunks {
            let client = client.clone();
            let url = format!("{}-chunk{i}", self.asset_url);

            threads.push(tokio::spawn(async move { client.get(url).send().await }));
        }

        for thread in threads {
            let res = thread.await??;
            let chunk = res.bytes().await?;
            file.append(&mut chunk.to_vec());
        }

        Ok(file)
    }
}

/// Creates a new release on GitHub and returns the `upload_url`.
/// If the release exists, it will only return the `upload_url`.
async fn create_or_get_release(repo: &str, tag: &str, client: Client) -> Result<String> {
    let url = format!("https://api.github.com/repos/{repo}/releases/tags/{tag}");
    let release = client
        .get(url)
        .send()
        .await?
        .json::<serde_json::Value>()
        .await?;

    if let Some(upload_url) = release.get("upload_url").and_then(|u| u.as_str()) {
        Ok(upload_url.to_string())
    } else {
        let url = format!("https://api.github.com/repos/{}/releases", repo);
        let release = client
            .post(url)
            .json(&serde_json::json!({
                "tag_name": tag,
                "name": "Files",
                "body": "",
                "draft": false,
                "prerelease": false
            }))
            .send()
            .await?
            .json::<serde_json::Value>()
            .await?;

        if let Some(upload_url) = release.get("upload_url").and_then(|u| u.as_str()) {
            Ok(upload_url.to_string())
        } else {
            // at this point, the repo is empty, so we need to create a file
            // to make it non-empty.
            let url = format!("https://api.github.com/repos/{repo}/contents/__no_empty_repo__",);
            client
                .put(url)
                .json(&serde_json::json!({
                    "message": "create file to allow creation of a release.",
                    "content": "",
                    "sha254": "",
                }))
                .send()
                .await?
                .text()
                .await?;

            let url = format!("https://api.github.com/repos/{}/releases", repo);
            let release = client
                .post(url)
                .json(&serde_json::json!({
                    "tag_name": tag,
                    "name": "Files",
                    "body": "",
                    "draft": false,
                    "prerelease": false
                }))
                .send()
                .await?
                .json::<serde_json::Value>()
                .await?;

            Ok(release
                .get("upload_url")
                .and_then(|u| u.as_str())
                .unwrap()
                .to_string())
        }
    }
}
