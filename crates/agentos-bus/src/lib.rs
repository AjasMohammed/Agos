pub mod client;
pub mod message;
pub mod server;
pub mod transport;

pub use client::BusClient;
pub use message::*;
pub use server::{BusConnection, BusServer};
pub use transport::{read_message, write_message};

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_server_client_roundtrip() {
        let dir = TempDir::new().unwrap();
        let sock_path = dir.path().join("test.sock");

        let server = BusServer::bind(&sock_path).await.unwrap();

        // Spawn server acceptor
        let server_handle = tokio::spawn(async move {
            let mut conn = server.accept().await.unwrap();
            let msg = conn.read().await.unwrap();
            match msg {
                BusMessage::Command(KernelCommand::GetStatus) => {
                    conn.write(&BusMessage::CommandResponse(KernelResponse::Status(
                        SystemStatus {
                            uptime_secs: 42,
                            connected_agents: 1,
                            active_tasks: 0,
                            installed_tools: 5,
                            total_audit_entries: 100,
                        },
                    )))
                    .await
                    .unwrap();
                }
                _ => panic!("Unexpected message"),
            }
        });

        // Client connects and sends a command
        let mut client = BusClient::connect(&sock_path).await.unwrap();
        let response = client.send_command(KernelCommand::GetStatus).await.unwrap();

        match response {
            KernelResponse::Status(status) => {
                assert_eq!(status.uptime_secs, 42);
                assert_eq!(status.connected_agents, 1);
            }
            _ => panic!("Unexpected response"),
        }

        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn test_large_message() {
        // Test that messages up to 16MB are handled correctly
        let dir = TempDir::new().unwrap();
        let sock_path = dir.path().join("test.sock");

        let server = BusServer::bind(&sock_path).await.unwrap();

        // Create a large payload
        let large_data = "x".repeat(1_000_000); // ~1MB string

        let server_handle = tokio::spawn(async move {
            let mut conn = server.accept().await.unwrap();
            let msg = conn.read().await.unwrap();
            conn.write(&msg).await.unwrap(); // echo back
        });

        let mut client = BusClient::connect(&sock_path).await.unwrap();
        let cmd = KernelCommand::RunTask {
            agent_name: Some("test".into()),
            prompt: large_data.clone(),
        };
        client
            .send_message(&BusMessage::Command(cmd))
            .await
            .unwrap();

        let response: BusMessage = client.receive_message().await.unwrap();
        // Verify round-trip integrity
        match response {
            BusMessage::Command(KernelCommand::RunTask { prompt, .. }) => {
                assert_eq!(prompt.len(), 1_000_000);
            }
            _ => panic!("Unexpected response"),
        }

        server_handle.await.unwrap();
    }
}
