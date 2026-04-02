use tokio::io::{AsyncBufRead, AsyncBufReadExt};

/// Maximum number of bytes accepted from a single MCP server response line.
/// Prevents memory exhaustion from a malicious or malfunctioning server.
pub const MAX_MCP_RESPONSE_BYTES: usize = 10 * 1024 * 1024; // 10 MB

/// Read a single newline-terminated line from `reader`, enforcing a byte limit
/// *during* the read rather than after. This prevents a malicious server from
/// exhausting memory by sending a very large payload without a newline.
///
/// Returns the number of bytes read (0 means EOF).
pub async fn read_line_limited(
    reader: &mut (impl AsyncBufRead + Unpin),
    buf: &mut String,
    max_bytes: usize,
) -> Result<usize, anyhow::Error> {
    let mut total = 0usize;
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            break; // EOF
        }
        let newline_pos = available.iter().position(|&b| b == b'\n');
        let chunk_end = newline_pos.map_or(available.len(), |p| p + 1);
        total += chunk_end;
        if total > max_bytes {
            anyhow::bail!("MCP server response exceeds {} byte limit", max_bytes);
        }
        let chunk = &available[..chunk_end];
        buf.push_str(
            std::str::from_utf8(chunk)
                .map_err(|e| anyhow::anyhow!("Invalid UTF-8 from MCP server: {e}"))?,
        );
        reader.consume(chunk_end);
        if newline_pos.is_some() {
            break; // found the line terminator
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use tokio::io::BufReader;

    #[tokio::test]
    async fn read_line_limited_normal() {
        let data = b"hello world\n";
        let mut reader = BufReader::new(Cursor::new(data));
        let mut buf = String::new();
        let n = read_line_limited(&mut reader, &mut buf, 1024)
            .await
            .unwrap();
        assert_eq!(n, 12);
        assert_eq!(buf, "hello world\n");
    }

    #[tokio::test]
    async fn read_line_limited_exceeds_limit() {
        let data = b"this is too long\n";
        let mut reader = BufReader::new(Cursor::new(data));
        let mut buf = String::new();
        let result = read_line_limited(&mut reader, &mut buf, 5).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("byte limit"));
    }

    #[tokio::test]
    async fn read_line_limited_eof() {
        let data = b"";
        let mut reader = BufReader::new(Cursor::new(data));
        let mut buf = String::new();
        let n = read_line_limited(&mut reader, &mut buf, 1024)
            .await
            .unwrap();
        assert_eq!(n, 0);
    }
}
