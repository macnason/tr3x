use anyhow::Result;
pub async fn run(thread: String) -> Result<()> {
    println!("chat {thread}");
    std::future::pending::<()>().await;
    Ok(())
}
