/// Hit the kernel's /healthz endpoint and exit 0 on success, 1 on failure.
/// This is used as the Docker HEALTHCHECK command — no bus connection needed.
pub async fn handle(port: u16) -> anyhow::Result<()> {
    let url = format!("http://localhost:{}/healthz", port);
    match reqwest::get(&url).await {
        Ok(resp) if resp.status().is_success() => std::process::exit(0),
        Ok(resp) => {
            eprintln!("healthz: HTTP {}", resp.status());
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("healthz: {}", e);
            std::process::exit(1);
        }
    }
}
