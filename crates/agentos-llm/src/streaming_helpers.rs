//! Streaming helpers for LLM adapters.
//!
//! Provides utilities for adapters that do not natively support streaming (e.g. mock,
//! or certain HTTP-only providers) to simulate token-by-token output.

use crate::types::InferenceEvent;
use std::time::Duration;
use tokio::sync::mpsc;

/// Simulate token streaming by chunking `text` into windows of `chunk_chars` characters
/// and sending each as an `InferenceEvent::Token` with a small delay between chunks.
///
/// This makes the UX feel alive even when the underlying adapter delivers the entire
/// response as a single string.
pub async fn simulate_token_stream(
    tx: &mpsc::Sender<InferenceEvent>,
    text: &str,
    chunk_chars: usize,
    delay: Duration,
) -> Result<(), mpsc::error::SendError<InferenceEvent>> {
    let chars: Vec<char> = text.chars().collect();
    for window in chars.chunks(chunk_chars) {
        let chunk: String = window.iter().collect();
        tx.send(InferenceEvent::Token(chunk)).await?;
        if delay > Duration::ZERO {
            tokio::time::sleep(delay).await;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_simulate_token_stream_chunks() {
        let (tx, mut rx) = mpsc::channel(100);
        let text = "Hello, world! This is a test string.";

        tokio::spawn(async move {
            simulate_token_stream(&tx, text, 10, Duration::ZERO)
                .await
                .unwrap();
        });

        let mut reassembled = String::new();
        let mut count = 0;
        while let Some(event) = rx.recv().await {
            if let InferenceEvent::Token(chunk) = event {
                reassembled.push_str(&chunk);
                count += 1;
            }
        }

        assert_eq!(reassembled, text);
        assert_eq!(count, 4); // 35 chars / 10 = 4 chunks (10+10+10+5)
    }

    #[tokio::test]
    async fn test_simulate_empty_string() {
        let (tx, mut rx) = mpsc::channel(10);

        tokio::spawn(async move {
            simulate_token_stream(&tx, "", 10, Duration::ZERO)
                .await
                .unwrap();
        });

        // Should receive nothing.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(rx.try_recv().is_err());
    }
}
