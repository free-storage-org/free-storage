use clap::Parser;
use free_storage::FileId;

fn validate_file_name(s: &str) -> Result<String, String> {
    if !std::path::Path::new(s).exists() {
        Err(format!("`{s}` doesn't exist."))
    } else {
        Ok(String::from(s))
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
    /// The name of the file to upload
    #[arg(value_parser = validate_file_name)]
    file_name: String,
    #[arg(value_parser = validate_repo)]
    /// Repository to put files in.
    ///
    /// Must be in `owner/repo` format.
    repo: String,
    /// A GitHub token to use to upload/retrieve files.
    ///
    /// Must have read and write access to the repository.
    token: String,
    /// The file to output the [`FileId`] in MessagePack format.
    output_file: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let Args {
        file_name,
        repo,
        token,
        output_file,
    } = Args::parse();

    let fid = FileId::upload_file(
        &file_name,
        &*std::fs::read(&file_name).unwrap(),
        repo,
        &token,
    )
    .await
    .unwrap();

    std::fs::write(output_file, rmp_serde::to_vec(&fid)?)?;

    Ok(())
}
