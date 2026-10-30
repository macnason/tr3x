use anyhow::Result;
pub async fn run(thread: String) -> Result<()> {
    println!("compose {thread}");
    std::future::pending::<()>().await;
    Ok(())
}
