use reqwest::{header, Client};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Clone, Debug)]
pub struct FileId {
    pub(crate) asset_url: String,
    pub(crate) chunks: usize,
}

#[tokio::main]
async fn main() {
    let _ = dotenv::dotenv();

    let repo = std::env::var("GITHUB_REPO").expect("GITHUB_REPO not set");
    let token = std::env::var("GITHUB_TOKEN").expect("GITHUB_TOKEN not set");

    let client = Client::builder()
        .default_headers({
            let mut map = header::HeaderMap::new();
            map.insert(
                header::AUTHORIZATION,
                header::HeaderValue::from_str(&format!("token {}", token)).unwrap(),
            );
            map.insert(
                header::USER_AGENT,
                header::HeaderValue::from_static("rust-app"),
            );
            map
        })
        .build()
        .unwrap();

    let id = upload_file(std::fs::read("test.txt").unwrap(), &repo, client.clone())
        .await
        .unwrap();

    std::fs::write("test1.txt", get_file(&id, client).await.unwrap()).unwrap();

    assert!(std::fs::read("test.txt").unwrap() == std::fs::read("test1.txt").unwrap());
}

pub async fn upload_file(file: Vec<u8>, repo: &str, client: Client) -> Result<FileId> {
    let upload_url = create_or_get_release(repo, "files", client.clone()).await?;
    let url = upload_url
        .strip_suffix("{?name,label}")
        .unwrap_or(&upload_url)
        .parse::<url::Url>()?;
    println!("creating hash");
    let hash = sha256::digest_bytes(&file);

    // split the file into chunks of ~100 megabytes
    // I know GitHub allows releases of 2 gigabytes, but it takes too long to upload that much data.
    let chunks = file.chunks(100_000_000);
    let chunks_len = chunks.len();

    let mut threads = Vec::with_capacity(chunks.len());

    for (i, chunk) in chunks.enumerate() {
        let mut url = url.clone();
        url.set_query(Some(&format!("name={hash}-chunk{i}")));

        let client = client.clone();
        let chunk = chunk.to_vec();
        threads.push(tokio::spawn(async move {
            println!("uploading chunk {i}");
            let res = client
                .post(url)
                .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
                .header(reqwest::header::CONTENT_LENGTH, chunk.len())
                .body(chunk)
                .send()
                .await;
            println!("uploaded chunk {i}");
            res
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

    Ok(FileId {
        asset_url,
        chunks: chunks_len,
    })
}

pub async fn get_file(file_id: &FileId, client: Client) -> Result<Vec<u8>> {
    let mut file = Vec::new();
    let mut threads = Vec::with_capacity(file_id.chunks);
    for i in 0..file_id.chunks {
        let url = format!("{}-chunk{i}", file_id.asset_url);

        let client = client.clone();
        threads.push(tokio::spawn(async move {
            println!("downloading chunk {i}");
            let res = client
                .get(url)
                .header(header::ACCEPT, "application/octet-stream")
                .send()
                .await;
            println!("downloaded chunk {i}");
            res
        }));
    }

    for thread in threads {
        let res = thread.await??;
        let chunk = res.bytes().await?;
        file.append(&mut chunk.to_vec());
    }

    Ok(file)
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
