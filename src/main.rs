use std::{
    fs,
    path::{Path, PathBuf},
};

use clap::Parser;
use free_storage::FileId;
use reqwest::header::ACCEPT;
use serde::Deserialize;
use serde_json::json;

fn validate_path(s: &str) -> Result<PathBuf, String> {
    let p = Path::new(s);
    if p.exists() {
        Ok(p.to_owned())
    } else {
        Err(format!("`{s}` doesn't exist."))
    }
}
fn validate_repo(s: &str) -> Result<String, String> {
    if s.split('/').count() != 2 {
        Err(String::from(
            "Invalid repository. Must be in `owner/repo` format.",
        ))
    } else {
        Ok(String::from(s))
    }
}

#[derive(Debug, Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[command(subcommand)]
    action: Action,
}

#[derive(Debug, clap::Subcommand)]
enum Action {
    /// Upload a file
    Upload {
        /// The path of the file to upload
        #[arg(value_parser = validate_path)]
        file_path: PathBuf,
        /// Repository to put files in
        ///
        /// Must be in `owner/repo` format
        #[arg(value_parser = validate_repo)]
        repo: String,
        /// A GitHub token to use to upload/retrieve files
        ///
        /// Must have read and write access to the repository
        #[arg(short, long)]
        token: Option<String>,
        /// The file to output the [`FileId`] in MessagePack format
        out_path: PathBuf,
    },
    /// Download a file
    Download {
        /// The filename of a [`FileId`] in MessagePack format
        #[arg(value_parser = validate_path)]
        fileid_path: PathBuf,
        /// The token to use to read the files
        #[arg(short, long)]
        token: Option<String>,
        /// Where we should place the downloaded file
        ///
        /// Defaults to the original filename
        #[arg(short, long)]
        out_path: Option<PathBuf>,
    },
    /// Login to GitHub
    Login {
        /// If you already have a token, use it here instead of using the oauth flow
        #[arg(short, long)]
        token: Option<String>,
    },
}

fn get_token(token: Option<String>) -> String {
    token.unwrap_or_else(|| {
        match fs::read_to_string(
            dirs::config_dir()
                .unwrap()
                .join(".free_storage")
                .join("token"),
        ) {
            Ok(s) => s,
            Err(_) => {
                tracing::error!("Please provide `--token` or run `storage login` first.");
                std::process::exit(1);
            }
        }
    })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().without_time().compact().init();

    match Args::parse().action {
        Action::Upload {
            file_path,
            repo,
            token,
            out_path,
        } => {
            let token = get_token(token);

            let fid = FileId::upload(
                &*file_path.file_name().unwrap().to_string_lossy(),
                &*fs::read(&file_path).unwrap(),
                repo,
                &token,
            )
            .await?;

            fs::write(out_path, rmp_serde::to_vec(&fid)?)?;
        }
        Action::Download {
            fileid_path,
            token,
            out_path,
        } => {
            let fid = rmp_serde::from_slice::<FileId>(&fs::read(fileid_path)?)?;
            let (data, name) = fid.get(token).await?;

            if let Some(path) = out_path {
                fs::write(path, data)?;
            } else {
                fs::write(Path::new(&name).file_name().unwrap(), data)?;
            }
        }
        Action::Login { token } => {
            fn set_token(token: String) -> Result<(), std::io::Error> {
                tracing::debug!("Setting token to {token}");
                let conf_dir = dirs::config_dir().unwrap().join(".free_storage");
                fs::create_dir_all(&conf_dir)?;
                fs::write(conf_dir.join("token"), token)
            }

            if let Some(token) = token {
                set_token(token)?;
                return Ok(());
            }

            let client = reqwest::Client::new();

            // https://docs.github.com/en/developers/apps/building-oauth-apps/authorizing-oauth-apps#response-parameters
            #[derive(Deserialize)]
            struct DeviceFlow {
                /// The device verification code is 40 characters and used to verify the device.
                device_code: String,
                /// The user verification code is displayed on the device so the user can enter the code in a browser. This code is 8 characters with a hyphen in the middle.
                user_code: String,
                /// The verification URL where users need to enter the `user_code`: `<https://github.com/login/device>`.
                verification_uri: String,
                /// The number of seconds before the `device_code` and `user_code` expire. The default is 900 seconds or 15 minutes.
                #[allow(dead_code)]
                expires_in: u64,
                /// The minimum number of seconds that must pass before you can make a new access token request (`POST <https://github.com/login/oauth/access_token>`) to complete the device authorization. For example, if the interval is 5, then you cannot make a new request until 5 seconds pass. If you make more than one request over 5 seconds, then you will hit the rate limit and receive a `slow_down` error.
                interval: u64,
            }

            let device_flow = &client
                .post("https://github.com/login/device/code")
                .header(ACCEPT, "application/json")
                .json(&json!({
                    // TODO: make this configurable
                    "client_id": "7b0a933c618e69b8f1b9",
                    "scope": "repo"
                }))
                .send()
                .await?
                .json::<DeviceFlow>()
                .await?;

            tracing::info!(
                "Go to {} and enter the code {}",
                device_flow.verification_uri,
                device_flow.user_code
            );

            // https://docs.github.com/en/developers/apps/building-oauth-apps/authorizing-oauth-apps#response-2
            #[derive(Debug, Deserialize)]
            #[serde(untagged)]
            enum AccessToken {
                Error { error: AccessError },
                Success { access_token: String },
            }

            // https://docs.github.com/en/developers/apps/building-oauth-apps/authorizing-oauth-apps#error-codes-for-the-device-flow
            #[derive(Debug, Deserialize)]
            #[serde(rename_all = "snake_case")]
            enum AccessError {
                /// This error occurs when the authorization request is pending and the user hasn't entered the user code yet. The app is expected to keep polling the `POST <https://github.com/login/oauth/access_token>` request without exceeding the `interval`, which requires a minimum number of seconds between each request.
                AuthorizationPending,
                /// When you receive the `slow_down` error, 5 extra seconds are added to the minimum `interval` or timeframe required between your requests using `POST <https://github.com/login/oauth/access_token>`. For example, if the starting interval required at least 5 seconds between requests and you get a `slow_down` error response, you must now wait a minimum of 10 seconds before making a new request for an OAuth access token. The error response includes the new `interval` that you must use.
                #[allow(dead_code)]
                SlowDown { interval: u8 },
                ///	When a user clicks cancel during the authorization process, you'll receive a `access_denied` error and the user won't be able to use the verification code again.
                AccessDenied,
            }

            let mut interval =
                tokio::time::interval(std::time::Duration::from_secs(device_flow.interval + 1));
            loop {
                interval.tick().await;

                let res = client
                    .post("https://github.com/login/oauth/access_token")
                    .header(ACCEPT, "application/json")
                    .json(&json!({
                        "client_id": "7b0a933c618e69b8f1b9",
                        "device_code": device_flow.device_code,
                        "grant_type": "urn:ietf:params:oauth:grant-type:device_code"
                    }))
                    .send()
                    .await?
                    .json::<AccessToken>()
                    .await?;

                match res {
                    AccessToken::Error {
                        error: AccessError::AccessDenied,
                    } => {
                        tracing::error!("You denied access to your GitHub account");
                        std::process::exit(1);
                    }
                    AccessToken::Error {
                        error: AccessError::AuthorizationPending,
                    } => {
                        tracing::trace!("Authorization pending");
                    }

                    AccessToken::Error {
                        error:
                            AccessError::SlowDown {
                                ..
                            },
                    } => unreachable!("We should never get a slow down error. Also, this shouldn't be parsable for Serde."),

                    AccessToken::Success { access_token, .. } => {
                        set_token(access_token)?;
                        break;
                    },
                };
            }
        }
    }

    Ok(())
}
