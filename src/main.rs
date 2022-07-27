use free_storage::FileId;

async fn real_main() {
    let mut args = std::env::args();
    args.next();
    let file_name = args.next().unwrap();
    let repo = args.next().unwrap();
    let token = args.next().unwrap();

    let fid = FileId::upload_file(&file_name, std::fs::read(&file_name).unwrap(), repo, &token)
        .await
        .unwrap();

    let (file_data, file_name) = fid.get_file(Some(token)).await.unwrap();

    println!("{}", String::from_utf8_lossy(&file_data));

    println!("{file_name}");
}

fn main() {
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(real_main());
}
