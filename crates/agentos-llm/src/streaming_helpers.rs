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

/// Append a raw byte `chunk` from an HTTP body stream to `out`, decoding UTF-8
/// across chunk boundaries.
///
/// HTTP chunks split at arbitrary byte offsets, so a multibyte character (e.g.
/// `é` = `C3 A9`) may straddle two chunks. Decoding each chunk independently
/// with `from_utf8_lossy` replaces the split bytes with U+FFFD and corrupts the
/// token. This keeps any incomplete trailing sequence in `pending` until the
/// next chunk completes it. Genuinely invalid bytes are replaced with U+FFFD
/// and skipped so a bad byte can never stall the stream.
pub fn push_utf8_chunk(pending: &mut Vec<u8>, chunk: &[u8], out: &mut String) {
    pending.extend_from_slice(chunk);
    // Cursor instead of repeated `drain` so a run of invalid bytes stays linear.
    let mut i = 0;
    loop {
        match std::str::from_utf8(&pending[i..]) {
            Ok(s) => {
                out.push_str(s);
                pending.clear();
                return;
            }
            Err(e) => {
                let valid = e.valid_up_to();
                // `..valid` is guaranteed valid UTF-8, so lossy never substitutes here.
                out.push_str(&String::from_utf8_lossy(&pending[i..i + valid]));
                match e.error_len() {
                    Some(bad) => {
                        out.push('\u{FFFD}');
                        i += valid + bad;
                    }
                    None => {
                        // Incomplete sequence at the tail: keep it (≤3 bytes) for the next chunk.
                        pending.drain(..i + valid);
                        return;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod utf8_tests {
    use super::push_utf8_chunk;

    #[test]
    fn multibyte_split_across_chunks_is_reassembled() {
        let bytes = "data: caf\u{e9} au lait\n".as_bytes();
        // Split inside `é` (C3 | A9).
        let split = bytes.iter().position(|&b| b == 0xC3).unwrap() + 1;
        let mut pending = Vec::new();
        let mut out = String::new();
        push_utf8_chunk(&mut pending, &bytes[..split], &mut out);
        assert_eq!(out, "data: caf");
        assert_eq!(pending, vec![0xC3]);
        push_utf8_chunk(&mut pending, &bytes[split..], &mut out);
        assert_eq!(out, "data: caf\u{e9} au lait\n");
        assert!(pending.is_empty());
        assert_eq!(out.matches("caf\u{e9}").count(), 1);
    }

    #[test]
    fn four_byte_emoji_split_three_ways() {
        let s = "a\u{1F600}b";
        let b = s.as_bytes(); // 61 F0 9F 98 80 62
        let mut pending = Vec::new();
        let mut out = String::new();
        push_utf8_chunk(&mut pending, &b[..2], &mut out);
        push_utf8_chunk(&mut pending, &b[2..4], &mut out);
        push_utf8_chunk(&mut pending, &b[4..], &mut out);
        assert_eq!(out, s);
    }

    #[test]
    fn long_invalid_run_is_linear_and_keeps_tail() {
        let mut bytes = vec![0xFF; 200_000];
        bytes.push(0xC3); // incomplete `é` at the end
        let mut pending = Vec::new();
        let mut out = String::new();
        push_utf8_chunk(&mut pending, &bytes, &mut out);
        assert_eq!(out.chars().count(), 200_000);
        assert!(out.chars().all(|c| c == '\u{FFFD}'));
        assert_eq!(pending, vec![0xC3]);
    }

    #[test]
    fn invalid_byte_is_replaced_not_stalled() {
        let mut pending = Vec::new();
        let mut out = String::new();
        push_utf8_chunk(&mut pending, b"ok\xFFok", &mut out);
        assert_eq!(out, "ok\u{FFFD}ok");
        assert!(pending.is_empty());
    }
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
