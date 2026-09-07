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
            autonomous: false,
            no_checkpoint: false,
            thinking_level: agentos_types::ThinkingLevel::Off,
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

    #[tokio::test]
    async fn test_bind_refuses_when_another_kernel_owns_socket() {
        let dir = TempDir::new().unwrap();
        let sock_path = dir.path().join("test.sock");

        let _first = BusServer::bind(&sock_path).await.unwrap();

        // `unwrap_err` would require `BusServer: Debug`; match instead so the
        // success case still fails loudly without widening the type's API.
        let err = match BusServer::bind(&sock_path).await {
            Ok(_) => panic!("second bind must be refused while the first server is live"),
            Err(e) => e.to_string(),
        };
        assert!(
            err.contains("Another AgentOS kernel is already listening"),
            "unexpected error: {err}"
        );

        // The live server's socket must survive the refused bind.
        assert!(sock_path.exists());
    }

    #[tokio::test]
    async fn test_bind_replaces_genuinely_stale_socket() {
        let dir = TempDir::new().unwrap();
        let sock_path = dir.path().join("test.sock");

        // std's UnixListener does not unlink on drop, so this leaves exactly the
        // kind of orphan a crashed kernel leaves behind.
        let stale = std::os::unix::net::UnixListener::bind(&sock_path).unwrap();
        drop(stale);
        assert!(sock_path.exists());

        let server = BusServer::bind(&sock_path).await.unwrap();
        // The new server is actually reachable.
        BusClient::connect(&sock_path).await.unwrap();
        drop(server);
    }

    #[tokio::test]
    async fn test_drop_does_not_remove_a_socket_we_no_longer_own() {
        let dir = TempDir::new().unwrap();
        let sock_path = dir.path().join("test.sock");

        let server = BusServer::bind(&sock_path).await.unwrap();

        // Simulate a second kernel replacing the socket file at the same path.
        std::fs::remove_file(&sock_path).unwrap();
        let usurper = std::os::unix::net::UnixListener::bind(&sock_path).unwrap();

        drop(server);
        assert!(
            sock_path.exists(),
            "Drop unlinked a socket owned by another server"
        );
        drop(usurper);
    }

    #[tokio::test]
    async fn test_server_push_does_not_desync_command_responses() {
        let dir = TempDir::new().unwrap();
        let sock_path = dir.path().join("test.sock");

        let server = BusServer::bind(&sock_path).await.unwrap();

        let server_handle = tokio::spawn(async move {
            let mut conn = server.accept().await.unwrap();

            // First command: answer with a push in front of the real response.
            let _ = conn.read().await.unwrap();
            conn.write(&BusMessage::StatusUpdate(StatusUpdate {
                task_id: agentos_types::TaskID::new(),
                state: agentos_types::TaskState::Running,
                message: "working".into(),
            }))
            .await
            .unwrap();
            conn.write(&BusMessage::CommandResponse(KernelResponse::TaskLogs(
                vec!["first".to_string()],
            )))
            .await
            .unwrap();

            // Second command must still get its own answer, not the first one's.
            let _ = conn.read().await.unwrap();
            conn.write(&BusMessage::CommandResponse(KernelResponse::TaskLogs(
                vec!["second".to_string()],
            )))
            .await
            .unwrap();
        });

        let mut client = BusClient::connect(&sock_path).await.unwrap();

        match client.send_command(KernelCommand::GetStatus).await.unwrap() {
            KernelResponse::TaskLogs(logs) => assert_eq!(logs, vec!["first".to_string()]),
            other => panic!("Unexpected response: {other:?}"),
        }
        match client.send_command(KernelCommand::GetStatus).await.unwrap() {
            KernelResponse::TaskLogs(logs) => assert_eq!(logs, vec!["second".to_string()]),
            other => panic!("Unexpected response: {other:?}"),
        }

        server_handle.await.unwrap();
    }
}
