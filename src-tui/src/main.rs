#[tokio::main]
async fn main() -> std::io::Result<()> {
    codeforge_tui::run_codeforge_main().await.map(|_| ())
}
