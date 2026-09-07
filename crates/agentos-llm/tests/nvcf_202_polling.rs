//! NVIDIA's NIM gateway defers long generations: the POST answers `202` with
//! an `NVCF-REQID` header and no body, and the completion has to be polled
//! from NVIDIA Cloud Functions. reqwest counts 202 as success, so without this
//! handling the adapter parses an empty body and fails the turn.

use agentos_llm::catalog::CatalogEntry;
use agentos_llm::{CustomCore, LLMCore};
use agentos_types::{ContentPart, ContextCategory, ContextEntry, ContextPartition, ContextRole};
use agentos_types::{ContextMetadata, ContextWindow};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const COMPLETION: &str = r#"{"choices":[{"message":{"content":"polled result"},"finish_reason":"stop"}],"usage":{"prompt_tokens":7,"completion_tokens":3,"total_tokens":10}}"#;

/// Serves `202 + NVCF-REQID` to the POST, then `202` once and `200` after on
/// the status GET, so the retry-until-ready loop is actually exercised.
/// Returns the bound address and how many status polls were served.
async fn spawn_gateway(pending_polls: usize) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let polls = Arc::new(AtomicUsize::new(0));
    let polls_for_task = polls.clone();

    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let polls = polls_for_task.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();

                let response = if req.starts_with("POST") {
                    "HTTP/1.1 202 Accepted\r\nNVCF-REQID: req-abc123\r\nContent-Length: 0\r\n\
                     Connection: close\r\n\r\n"
                        .to_string()
                } else if req.contains("/v2/nvcf/pexec/status/req-abc123") {
                    let seen = polls.fetch_add(1, Ordering::SeqCst);
                    if seen < pending_polls {
                        "HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            .to_string()
                    } else {
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                             Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                            COMPLETION.len(),
                            COMPLETION
                        )
                    }
                } else {
                    "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .to_string()
                };
                let _ = sock.write_all(response.as_bytes()).await;
                let _ = sock.flush().await;
            });
        }
    });

    (addr, polls)
}

fn user_context(text: &str) -> ContextWindow {
    let mut ctx = ContextWindow::new(8);
    ctx.push(ContextEntry {
        role: ContextRole::User,
        parts: vec![ContentPart::Text { text: text.into() }],
        timestamp: chrono::Utc::now(),
        metadata: None::<ContextMetadata>,
        importance: 0.5,
        pinned: false,
        reference_count: 0,
        partition: ContextPartition::Active,
        category: ContextCategory::Task,
        is_summary: false,
    });
    ctx
}

fn adapter_for(addr: &str, status_template: Option<String>) -> CustomCore {
    CustomCore::new(None, "m".into(), format!("http://{addr}/v1")).with_catalog_overrides(
        &CatalogEntry {
            name: "nim-test".into(),
            context_window: Some(100_000),
            status_url_template: status_template,
            ..Default::default()
        },
    )
}

#[tokio::test]
async fn deferred_202_is_polled_until_the_completion_is_ready() {
    let (addr, polls) = spawn_gateway(1).await;
    let adapter = adapter_for(
        &addr,
        Some(format!("http://{addr}/v2/nvcf/pexec/status/{{id}}")),
    );

    let result = adapter
        .infer(&user_context("hi"))
        .await
        .expect("202 must resolve into a completion");

    assert_eq!(result.text, "polled result");
    assert_eq!(result.tokens_used.total_tokens, 10);
    // One 202 poll, then the 200.
    assert_eq!(polls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn deferred_202_without_a_configured_poll_url_is_not_polled() {
    // No template → the 202 is left to the normal response path, which reports
    // the missing body rather than silently inventing a poll URL.
    let (addr, polls) = spawn_gateway(0).await;
    let adapter = adapter_for(&addr, None);

    let err = adapter
        .infer(&user_context("hi"))
        .await
        .expect_err("empty 202 body cannot produce a completion");

    assert!(
        err.to_string().contains("parse JSON response"),
        "unexpected error: {err}"
    );
    assert_eq!(polls.load(Ordering::SeqCst), 0);
}
