use std::time::Duration;

pub async fn wait_for_port(host: &str, port: u16, attempts: usize) -> bool {
    for _ in 0..attempts {
        if tokio::net::TcpStream::connect((host, port)).await.is_ok() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    false
}
