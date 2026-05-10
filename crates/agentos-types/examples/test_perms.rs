use agentos_types::capability::{PermissionOp, PermissionSet};

fn main() {
    let mut perms = PermissionSet::new();
    perms.grant("*".to_string(), true, true, true, None);
    perms.grant_op("*".to_string(), PermissionOp::Query, None);
    perms.grant_op("*".to_string(), PermissionOp::Observe, None);

    // Simulate role merging
    let mut role_perms = PermissionSet::new();
    role_perms.grant("fs.user_data".to_string(), true, true, false, None);

    let mut effective = PermissionSet::new();
    for e in perms.entries() {
        effective.grant_entry(e);
    }
    for e in role_perms.entries() {
        effective.grant_entry(e);
    }

    let exec_ok = effective.check("process.exec", PermissionOp::Execute);
    let write_ok = effective.check("fs.user_data", PermissionOp::Write);
    println!(
        "CHECK RESULTS: process.exec: Execute = {}, fs.user_data: Write = {}",
        exec_ok, write_ok
    );
}
