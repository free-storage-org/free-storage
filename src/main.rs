use std::{
    fs,
    path::{Path, PathBuf},
};

use clap::Parser;
use free_storage::FileId;

fn validate_path(s: &str) -> Result<PathBuf, String> {
    let p = Path::new(s);
    if !p.exists() {
        Err(format!("`{s}` doesn't exist."))
    } else {
        Ok(p.to_owned())
    }
}
fn validate_repo(s: &str) -> Result<String, String> {
    if s.split('/').count() != 2 && !s.contains("github.com") {
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
        #[arg(value_parser = validate_repo)]
        /// Repository to put files in
        ///
        /// Must be in `owner/repo` format
        repo: String,
        /// A GitHub token to use to upload/retrieve files
        ///
        /// Must have read and write access to the repository
        token: String,
        /// The file to output the [`FileId`] in MessagePack format
        output_path: PathBuf,
    },
    /// Download a file
    Download {
        /// The filename of a [`FileId`] in MessagePack format
        #[arg(value_parser = validate_path)]
        fileid_path: PathBuf,
        /// The token to use to read the files
        #[arg(short, long)]
        token: Option<String>,
        /// The output path to use
        ///
        /// Defaults to the original filename
        #[arg(short, long)]
        output_path: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    match Args::parse().action {
        Action::Upload {
            file_path,
            repo,
            token,
            output_path,
        } => {
            let fid = FileId::upload_file(
                &*file_path.file_name().unwrap().to_string_lossy(),
                &*fs::read(&file_path).unwrap(),
                repo,
                &token,
            )
            .await?;

            fs::write(output_path, rmp_serde::to_vec(&fid)?)?;
        }
        Action::Download {
            fileid_path,
            token,
            output_path,
        } => {
            let fid = rmp_serde::from_slice::<FileId>(&fs::read(fileid_path)?)?;
            let (data, name) = fid.get_file(token).await?;

            if let Some(path) = output_path {
                fs::write(path, data)?;
            } else {
                fs::write(Path::new(&name).file_name().unwrap(), data)?;
            }
        }
    }

    Ok(())
}
