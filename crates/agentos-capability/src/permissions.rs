use agentos_types::{AgentOSError, PermissionEntry};

/// Parse a permission string like "resource:rwx" into a PermissionEntry.
pub fn parse_permission_str(s: &str) -> Result<PermissionEntry, AgentOSError> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 2 {
        return Err(AgentOSError::SchemaValidation(format!(
            "Invalid permission format '{}', expected 'resource:rwx'",
            s
        )));
    }

    let resource = parts[0].to_string();
    let bits = parts[1];
    let read = bits.contains('r');
    let write = bits.contains('w');
    let execute = bits.contains('x');

    if !read && !write && !execute {
        return Err(AgentOSError::SchemaValidation(
            "Permission bits must contain at least one of r, w, x".to_string(),
        ));
    }

    Ok(PermissionEntry {
        resource,
        read,
        write,
        execute,
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

        let entry = parse_permission_str("network.outbound:x").unwrap();
        assert!(!entry.read);
        assert!(!entry.write);
        assert!(entry.execute);

        assert!(parse_permission_str("invalid").is_err());
        assert!(parse_permission_str("resource:").is_err());
    }
}
