use std::time::Duration;

/// Poll a runtime endpoint until it responds over HTTP, with TCP fallback.
///
/// `interval_ms` controls the pause between probes.
/// `attempts` controls the maximum number of probes.
pub async fn wait_for_port(host: &str, port: u16, attempts: usize, interval_ms: u64) -> bool {
    let url = format!("http://{host}:{port}/");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(500))
        .build()
        .ok();

    for _ in 0..attempts {
        if let Some(client) = &client {
            if client.get(&url).send().await.is_ok() {
                return true;
            }
        }

        // Keep a TCP fallback so services that reject "/" still become healthy
        // as soon as they start accepting connections.
        if tokio::net::TcpStream::connect((host, port)).await.is_ok() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(interval_ms)).await;
    }

    false
}
