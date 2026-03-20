use agentos_types::{AgentOSError, PermissionEntry};

/// Parse a permission string like "resource:rwxqo" into a PermissionEntry.
///
/// Supported flag characters: `r`=Read, `w`=Write, `x`=Execute, `q`=Query, `o`=Observe.
pub fn parse_permission_str(s: &str) -> Result<PermissionEntry, AgentOSError> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 2 {
        return Err(AgentOSError::SchemaValidation(format!(
            "Invalid permission format '{}', expected 'resource:BITS' where BITS is a combination of r,w,x,q,o",
            s
        )));
    }

    let resource = parts[0].to_string();
    let bits = parts[1];

    // Reject unknown flag characters to prevent silent misconfigurations.
    for ch in bits.chars() {
        if !matches!(ch, 'r' | 'w' | 'x' | 'q' | 'o') {
            return Err(AgentOSError::SchemaValidation(format!(
                "Unknown permission flag '{}' in '{}'; expected r, w, x, q, o",
                ch, s
            )));
        }
    }

    let read = bits.contains('r');
    let write = bits.contains('w');
    let execute = bits.contains('x');
    let query = bits.contains('q');
    let observe = bits.contains('o');

    if !read && !write && !execute && !query && !observe {
        return Err(AgentOSError::SchemaValidation(
            "Permission bits must contain at least one of r, w, x, q, o".to_string(),
        ));
    }

    Ok(PermissionEntry {
        resource,
        read,
        write,
        execute,
        query,
        observe,
        expires_at: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_permission_str() {
        let entry = parse_permission_str("fs.user_data:rw").unwrap();
        assert_eq!(entry.resource, "fs.user_data");
        assert!(entry.read);
        assert!(entry.write);
        assert!(!entry.execute);
        assert!(!entry.query);
        assert!(!entry.observe);

        let entry = parse_permission_str("network.outbound:x").unwrap();
        assert!(!entry.read);
        assert!(!entry.write);
        assert!(entry.execute);

        let entry = parse_permission_str("memory.semantic:q").unwrap();
        assert!(entry.query);
        assert!(!entry.read);

        let entry = parse_permission_str("events.stream:o").unwrap();
        assert!(entry.observe);
        assert!(!entry.query);

        let entry = parse_permission_str("memory.semantic:rq").unwrap();
        assert!(entry.read);
        assert!(entry.query);

        assert!(parse_permission_str("invalid").is_err());
        assert!(parse_permission_str("resource:").is_err());
        // Unknown flags must be rejected
        assert!(parse_permission_str("fs.data:rz").is_err());
        assert!(parse_permission_str("fs.data:e").is_err());
    }
}
