use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

use crate::hal::HalDriver;
use crate::types::{SocketEntry, SocketsResult};

pub struct NetworkSocketsDriver;

impl Default for NetworkSocketsDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl NetworkSocketsDriver {
    pub fn new() -> Self {
        Self
    }

    #[cfg(target_os = "linux")]
    pub fn list_sockets(&self, params: Value) -> Result<SocketsResult, AgentOSError> {
        use procfs::net::{tcp, tcp6, udp, udp6};

        let protocol = params
            .get("protocol")
            .and_then(|v| v.as_str())
            .unwrap_or("all");
        let state_filter = params
            .get("state")
            .and_then(|v| v.as_str())
            .unwrap_or("all");
        let ipv = params.get("ipv").and_then(|v| v.as_str()).unwrap_or("both");
        let port_filter = params
            .get("port")
            .and_then(|v| v.as_u64())
            .map(|v| v as u16);
        let include_pid = params
            .get("include_pid")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(200) as usize;

        let mut sockets = Vec::new();

        fn collect_tcp(
            sockets: &mut Vec<SocketEntry>,
            entries: Vec<procfs::net::TcpNetEntry>,
            version: &str,
            state_filter: &str,
            port_filter: Option<u16>,
        ) {
            for entry in entries {
                let state_str = format!("{:?}", entry.state).to_uppercase();
                if state_filter != "all" {
                    if state_filter == "listen" && state_str != "LISTEN" {
                        continue;
                    }
                    if state_filter == "established" && state_str != "ESTABLISHED" {
                        continue;
                    }
                }
                if let Some(port) = port_filter {
                    if entry.local_address.port() != port {
                        continue;
                    }
                }

                sockets.push(SocketEntry {
                    protocol: "tcp".to_string(),
                    ip_version: version.to_string(),
                    local_addr: entry.local_address.to_string(),
                    remote_addr: entry.remote_address.to_string(),
                    state: state_str,
                    inode: entry.inode,
                    pid: None,
                    process_name: None,
                });
            }
        }

        fn collect_udp(
            sockets: &mut Vec<SocketEntry>,
            entries: Vec<procfs::net::UdpNetEntry>,
            version: &str,
            state_filter: &str,
            port_filter: Option<u16>,
        ) {
            for entry in entries {
                if let Some(port) = port_filter {
                    if entry.local_address.port() != port {
                        continue;
                    }
                }
                if state_filter == "established" {
                    continue;
                }

                sockets.push(SocketEntry {
                    protocol: "udp".to_string(),
                    ip_version: version.to_string(),
                    local_addr: entry.local_address.to_string(),
                    remote_addr: entry.remote_address.to_string(),
                    state: "N/A".to_string(),
                    inode: entry.inode,
                    pid: None,
                    process_name: None,
                });
            }
        }

        if protocol == "all" || protocol == "tcp" {
            if ipv == "both" || ipv == "v4" {
                if let Ok(entries) = tcp() {
                    collect_tcp(&mut sockets, entries, "v4", state_filter, port_filter);
                }
            }
            if ipv == "both" || ipv == "v6" {
                if let Ok(entries) = tcp6() {
                    collect_tcp(&mut sockets, entries, "v6", state_filter, port_filter);
                }
            }
        }
        if protocol == "all" || protocol == "udp" {
            if ipv == "both" || ipv == "v4" {
                if let Ok(entries) = udp() {
                    collect_udp(&mut sockets, entries, "v4", state_filter, port_filter);
                }
            }
            if ipv == "both" || ipv == "v6" {
                if let Ok(entries) = udp6() {
                    collect_udp(&mut sockets, entries, "v6", state_filter, port_filter);
                }
            }
        }

        let total_matched = sockets.len();
        sockets.truncate(limit);

        if include_pid && !sockets.is_empty() {
            // Reverse map inode -> (pid, name). Bound the walk to inodes we
            // actually need: collect target inodes up front; break out of the
            // per-process fd loop early once all are resolved.
            let mut needed: HashSet<u64> = sockets.iter().map(|s| s.inode).collect();
            let mut inode_map: HashMap<u64, (u32, Option<String>)> = HashMap::new();

            match procfs::process::all_processes() {
                Ok(all_procs) => {
                    'outer: for p_res in all_procs {
                        // Per-process race tolerance: dead PIDs disappear mid-walk,
                        // skip individual errors without aborting the walk.
                        let p = match p_res {
                            Ok(p) => p,
                            Err(_) => continue,
                        };
                        let pid = p.pid() as u32;
                        let name = p.stat().ok().map(|s| s.comm);
                        let fds = match p.fd() {
                            Ok(f) => f,
                            Err(_) => continue,
                        };
                        for fd_res in fds {
                            let fd = match fd_res {
                                Ok(f) => f,
                                Err(_) => continue,
                            };
                            if let procfs::process::FDTarget::Socket(inode) = fd.target {
                                if needed.remove(&inode) {
                                    inode_map.insert(inode, (pid, name.clone()));
                                    if needed.is_empty() {
                                        break 'outer;
                                    }
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::debug!(
                        error = %e,
                        "network-sockets: procfs all_processes() failed; PIDs unresolved"
                    );
                }
            }

            for s in &mut sockets {
                if let Some((pid, name)) = inode_map.get(&s.inode) {
                    s.pid = Some(*pid);
                    s.process_name = name.clone();
                }
            }
        }

        let returned = sockets.len();
        Ok(SocketsResult {
            sockets,
            total_matched,
            returned,
        })
    }

    #[cfg(not(target_os = "linux"))]
    pub fn list_sockets(&self, _params: Value) -> Result<SocketsResult, AgentOSError> {
        Err(AgentOSError::HalError(
            "network-sockets not supported on this platform".into(),
        ))
    }
}

#[async_trait]
impl HalDriver for NetworkSocketsDriver {
    fn name(&self) -> &str {
        "network_sockets"
    }

    fn required_permission(&self) -> (&str, PermissionOp) {
        ("network.sockets", PermissionOp::Read)
    }

    async fn query(&self, params: Value) -> Result<Value, AgentOSError> {
        let result = self.list_sockets(params)?;
        Ok(serde_json::to_value(result).map_err(|e| AgentOSError::HalError(e.to_string()))?)
    }
}

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn test_list_tcp_listeners_returns_at_least_one() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let driver = NetworkSocketsDriver::new();
        let params = serde_json::json!({
            "protocol": "tcp",
            "state": "listen",
            "port": port
        });

        let res = driver.list_sockets(params).unwrap();
        assert!(res
            .sockets
            .iter()
            .any(|s| s.local_addr.contains(&port.to_string())));

        let our_socket = res
            .sockets
            .iter()
            .find(|s| s.local_addr.contains(&port.to_string()))
            .unwrap();
        assert_eq!(our_socket.pid, Some(std::process::id() as u32));
    }
}
