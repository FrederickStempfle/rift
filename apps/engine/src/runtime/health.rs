use std::time::Duration;

/// Poll a TCP port until it accepts a connection.
///
/// `interval_ms` controls the pause between probes (default was 500 ms, now
/// configurable via `RIFT_HEALTHCHECK_INTERVAL_MS`).
/// `attempts` controls the maximum number of probes (default was 40, now
/// configurable via `RIFT_HEALTHCHECK_ATTEMPTS`).
pub async fn wait_for_port(host: &str, port: u16, attempts: usize, interval_ms: u64) -> bool {
    for _ in 0..attempts {
        if tokio::net::TcpStream::connect((host, port)).await.is_ok() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(interval_ms)).await;
    }

    false
}
