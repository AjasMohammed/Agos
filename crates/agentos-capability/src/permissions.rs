use agentos_types::{AgentOSError, PermissionEntry, PermissionOp, PermissionSet};

/// Parse a permission string like "resource:rwxqo" into a PermissionEntry.
///
/// Supported flag characters: `r`=Read, `w`=Write, `x`=Execute, `q`=Query, `o`=Observe.
pub fn parse_permission_str(s: &str) -> Result<PermissionEntry, AgentOSError> {
    // Resources may contain colons (`fs:agents/<name>/`); bits follow the last one.
    let Some((resource, bits)) = s.rsplit_once(':').filter(|(r, _)| !r.is_empty()) else {
        return Err(AgentOSError::SchemaValidation(format!(
            "Invalid permission format '{}', expected 'resource:BITS' where BITS is a combination of r,w,x,q,o",
            s
        )));
    };
    let resource = resource.to_string();

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

/// Does `permissions` satisfy the manifest permission string `perm_str`
/// (e.g. `"fs.user_data:r"`, `"memory.semantic:rw"`)?
///
/// Every op flag in the string must be granted. A malformed string fails
/// closed — an unparseable requirement is never treated as satisfied.
pub fn permission_str_granted(permissions: &PermissionSet, perm_str: &str) -> bool {
    // `"*"` is the wildcard requirement ("any grant will do"). An EMPTY string
    // is not — it is a malformed entry (a TOML typo like `permissions = ["",
    // "process.exec:x"]`) and must fail closed like every other malformed
    // form, or it makes the tool universally visible via the any-of path. A
    // genuinely permission-free tool has an empty *list*, handled in
    // `any_permission_granted`.
    if perm_str == "*" {
        return true;
    }
    let Ok(entry) = parse_permission_str(perm_str) else {
        return false;
    };
    let res = &entry.resource;
    let ops = [
        (entry.read, PermissionOp::Read),
        (entry.write, PermissionOp::Write),
        (entry.execute, PermissionOp::Execute),
        (entry.query, PermissionOp::Query),
        (entry.observe, PermissionOp::Observe),
    ];
    ops.iter()
        .filter(|(wanted, _)| *wanted)
        .all(|(_, op)| permissions.check(res, *op))
}

/// Can the agent do *anything* with a tool declaring `required`?
///
/// Visibility test, not an enforcement test. A manifest lists the **union** of
/// every permission the tool can ever need (`webcam` declares both
/// `hardware.webcam.list:r` and `hardware.webcam.capture:x`), while
/// `AgentTool::required_permissions_for` narrows that per payload at call
/// time — so requiring the whole union here would hide tools the agent can
/// legitimately use in a narrower mode. A tool is hidden only when the agent
/// holds none of its declared permissions.
///
/// An empty `required` means the tool declares no permissions: always visible.
pub fn any_permission_granted(permissions: &PermissionSet, required: &[String]) -> bool {
    required.is_empty()
        || required
            .iter()
            .any(|p| permission_str_granted(permissions, p))
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

#[cfg(test)]
mod visibility_tests {
    use super::*;

    fn perms(resource: &str, read: bool, write: bool, execute: bool) -> PermissionSet {
        let mut p = PermissionSet::new();
        p.grant(resource.to_string(), read, write, execute, None);
        p
    }

    #[test]
    fn every_flag_in_the_string_must_be_granted() {
        let read_only = perms("fs.user_data", true, false, false);
        assert!(permission_str_granted(&read_only, "fs.user_data:r"));
        assert!(!permission_str_granted(&read_only, "fs.user_data:rw"));
    }

    #[test]
    fn malformed_permission_string_fails_closed() {
        let mut p = PermissionSet::new();
        p.grant("*".to_string(), true, true, true, None);
        assert!(!permission_str_granted(&p, "fs.user_data"));
        assert!(!permission_str_granted(&p, "fs.user_data:z"));
        // An empty entry inside a non-empty list is a typo, not a wildcard.
        assert!(!permission_str_granted(&p, ""));
        assert!(!any_permission_granted(
            &PermissionSet::new(),
            &["".to_string()]
        ));
    }

    /// A manifest lists the union of everything the tool can need (`webcam`
    /// declares list + capture), while the call-time check narrows per payload.
    /// Holding one of them keeps the tool visible.
    #[test]
    fn any_of_the_declared_permissions_keeps_a_tool_visible() {
        let list_only = perms("hardware.webcam.list", true, false, false);
        let declared = vec![
            "hardware.webcam.list:r".to_string(),
            "hardware.webcam.capture:x".to_string(),
        ];
        assert!(any_permission_granted(&list_only, &declared));
        assert!(!any_permission_granted(&PermissionSet::new(), &declared));
    }

    #[test]
    fn a_tool_declaring_nothing_is_always_visible() {
        assert!(any_permission_granted(&PermissionSet::new(), &[]));
    }
}
