use followme::FollowFile;
use std::env;

#[tokio::main]
async fn main() {
    use futures::StreamExt;

    let args: Vec<String> = env::args().collect();

    if args.len() <= 1 {
        println!("Usage: followme FILE");
        return;
    }

    let path = &args[1];
    let mut file = FollowFile::new(path.into()).await.unwrap();

    while let Some(line) = file.next().await {
        println!("{:?}", line);
    }
}
