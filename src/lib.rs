#![warn(clippy::nursery, clippy::pedantic)]
#![allow(clippy::missing_panics_doc, clippy::must_use_candidate)]

use serde_json::json;
use std::io::{ErrorKind, Read};
use uuid::Uuid;

use reqwest::{header, Client, Url};
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Network Error: {0}")]
    Reqwest(#[from] reqwest::Error),
    #[error("Error Parsing JSON: {0}")]
    JSON(#[from] serde_json::Error),
    #[error("I/O Error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Error Parsing URL: {0}")]
    Url(#[from] url::ParseError),
    #[error("Invalid Repository OR The Token is Invalid")]
    InvalidRepoOrInvalidToken,
    #[error("Unauthorized")]
    Unauthorized,
}

type Result<T> = std::result::Result<T, Error>;

/// A struct that holds the data for a single file.
#[allow(clippy::unsafe_derive_deserialize)]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileId {
    asset_ids: Vec<u32>,
    repo: String,
}

#[derive(Deserialize)]
struct AssetsResponse {
    id: u32,
}

impl FileId {
    /// Creates a new [`FileId`] from raw `asset_ids` and a `repo`.
    ///
    /// This usually isn't used, instead use [`Self::upload`].
    pub fn from_raw(asset_ids: Vec<u32>, repo: String) -> Self {
        Self { asset_ids, repo }
    }

    /// Uploads a file to the GitHub repository's releases.
    ///
    /// The token must have read and write access to the repository.
    /// `repo` must be in the format `owner/repo`.
    ///
    /// # Errors
    /// Returns an [`Error::InvalidRepo`] if `repo` is not in the correct format, it doesn't exist,
    /// or if the token does not have `read`/`write` access to the repository.
    pub async fn upload<S: Into<String> + Send + Sync>(
        file_name: S,
        mut file_data: impl Read + Send + Sync,
        repo: impl Into<String> + Send + Sync,
        token: impl AsRef<str> + Send + Sync,
    ) -> Result<Self> {
        let file_name = <S as Into<String>>::into(file_name)
            .chars()
            .filter(|&c| c != '?' && c != '!')
            .collect::<String>();
        let repo = repo.into();

        if repo.split('/').count() != 2 {
            return Err(Error::InvalidRepoOrInvalidToken);
        }

        tracing::debug!("Uploading file {file_name} to GitHub repo {repo}");

        let client = client(Some(token));

        let (_, uploads_url) = create_or_get_release(&repo, "files", client.clone()).await?;

        let uuid = Uuid::new_v4();

        let mut threads = Vec::new();

        let mut chunks = 0;

        loop {
            let mut url = uploads_url.clone();
            url.set_query(Some(&format!("name={uuid}-chunk{chunks}")));

            let client = client.clone();

            tracing::trace!("Reading chunk {chunks}");

            let mut chunk = {
                // We're only using 100 megabytes because of the time it takes to upload to GitHub
                let mut chunk = vec![0; 100_000_000];

                let read = loop {
                    match file_data.read(&mut chunk) {
                        Ok(a) => break a,
                        Err(e) => {
                            if e.kind() == ErrorKind::WouldBlock {
                                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                            } else {
                                return Err(e.into());
                            }
                        }
                    };
                };

                if read == 0 {
                    break;
                }
                if read < 100_000_000 {
                    tracing::trace!("Resizing chunk {chunks} from 100,000,000 to {read}");
                    // Don't keep all the trailing NULL bytes
                    chunk.splice(..read, []).collect()
                } else {
                    chunk
                }
            };

            if chunks == 0 {
                unsafe { prepend_slice(&mut chunk, format!("{file_name}?").as_bytes()) }
            }

            threads.push(tokio::spawn(async move {
                tracing::debug!(
                    "Uploading chunk {chunks} with {} bytes to {url}",
                    chunk.len()
                );
                client
                    .post(url)
                    .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
                    .body(chunk)
                    .send()
                    .await
            }));

            chunks += 1;
        }

        let mut asset_ids = Vec::with_capacity(chunks);
        for thread in threads {
            let json = thread.await.unwrap()?.json::<AssetsResponse>().await?;

            asset_ids.push(json.id);
        }

        Ok(Self { asset_ids, repo })
    }

    /// Downloads the file from the GitHub repository's releases.
    ///
    /// The token must have read access to the repository.
    ///
    /// # Errors
    /// Returns an [`Error::Unauthorized`] if the token does not have read access to the repository
    /// or if the file doesn't exist.
    ///
    /// Returns an [`Error::Reqwest`] if there was a network error.
    pub async fn get<T: Into<String> + Sync + Send>(
        &self,
        token: Option<T>,
    ) -> Result<(Vec<u8>, String)> {
        let chunks = self.asset_ids.len();

        tracing::debug!("Downloading {chunks} chunks");

        let mut file = Vec::<u8>::new();
        let mut threads = Vec::with_capacity(chunks);

        let client = client(token.map(Into::into));

        for asset_id in &self.asset_ids {
            let url = format!(
                "https://api.github.com/repos/{}/releases/assets/{asset_id}",
                self.repo
            );

            let client = client.clone();

            threads.push(tokio::spawn(async move {
                client
                    .get(url)
                    .header(header::ACCEPT, "application/octet-stream")
                    .send()
                    .await
            }));
        }

        for thread in threads {
            let res = thread.await.unwrap()?;

            if res.status().as_u16() == 404 {
                return Err(Error::Unauthorized);
            }

            let chunk = res.bytes().await?;
            file.extend(&chunk);
        }

        let file = file.into_iter();

        let file_name = file
            .clone()
            .map(|b| b as char)
            .take_while(|&c| c != '?')
            .collect::<String>();

        let file = file.skip(file_name.len() + 1).collect::<Vec<_>>();

        Ok((file, file_name))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ReleaseResponse {
    upload_url: Option<String>,
    assets_url: Option<String>,
}

/// Creates a new release on GitHub and returns the `assets_url`.
/// If the release exists, it will only return the `assets_url`.
async fn create_or_get_release(repo: &str, tag: &str, client: Client) -> Result<(Url, Url)> {
    let get_release = || async {
        let url = format!("https://api.github.com/repos/{repo}/releases/tags/{tag}");

        tracing::trace!("Getting release at {url}");

        let release = client
            .get(url)
            .send()
            .await?
            .json::<ReleaseResponse>()
            .await?;

        Result::Ok(
            release
                .assets_url
                .and_then(|a| release.upload_url.map(|u| (a, u)))
                .map(|(a, u)| {
                    let url = parse_url(&a).unwrap();
                    let upload_url = parse_url(&u).unwrap();
                    (url, upload_url)
                }),
        )
    };
    let create_release = || async {
        let url = format!("https://api.github.com/repos/{repo}/releases");

        tracing::trace!("Creating release at {url} with tag {tag}");

        let release = client
            .post(url)
            .json(&json!({
                "tag_name": tag,
            }))
            .send()
            .await?
            .json::<ReleaseResponse>()
            .await?;

        Result::Ok(
            release
                .assets_url
                .and_then(|a| release.upload_url.map(|u| (a, u)))
                .map(|(a, u)| {
                    let url = parse_url(&a).unwrap();
                    let upload_url = parse_url(&u).unwrap();
                    (url, upload_url)
                }),
        )
    };

    if let Ok(Some(urls)) = get_release().await {
        Ok(urls)
    } else if let Ok(Some(urls)) = create_release().await {
        Ok(urls)
    } else {
        // at this point, the repo probably has no commits.
        let url = format!("https://api.github.com/repos/{repo}/contents/__no_empty_repo__",);
        client
            .put(url)
            .json(&json!({
                "message": "add a commit to allow creation of a release.",
                "content": "",
                "sha254": "",
            }))
            .send()
            .await?
            .text()
            .await?;

        if let Ok(Some(urls)) = create_release().await {
            Ok(urls)
        } else {
            tracing::debug!(
                "Could not create release. This could be because:
                                                            *   The repo doesn't exist
                                                            *   The token is invalid"
            );
            Err(Error::InvalidRepoOrInvalidToken)
        }
    }
}

unsafe fn prepend_slice<T: Copy>(vec: &mut Vec<T>, slice: &[T]) {
    let len = vec.len();
    let amt = slice.len();
    vec.reserve(amt);

    std::ptr::copy(vec.as_ptr(), vec.as_mut_ptr().add(amt), len);
    std::ptr::copy(slice.as_ptr(), vec.as_mut_ptr(), amt);
    vec.set_len(len + amt);
}

fn client(token: Option<impl AsRef<str>>) -> Client {
    let client = Client::builder().user_agent("Rust").default_headers({
        let mut map = header::HeaderMap::new();
        if let Some(token) = token {
            map.insert(header::AUTHORIZATION, {
                let mut header =
                    header::HeaderValue::from_str(&format!("token {}", token.as_ref())).unwrap();
                header.set_sensitive(true);
                header
            });
        }
        map
    });
    client.build().unwrap()
}

fn parse_url(url: &str) -> Result<Url> {
    let mut url = url.parse::<Url>()?;
    url.set_query(None);

    if let Some(path) = url.clone().path().strip_suffix("%7B") {
        url.set_path(path);
    }

    Ok(url)
}
