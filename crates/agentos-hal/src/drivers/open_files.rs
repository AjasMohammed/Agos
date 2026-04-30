use agentos_types::{AgentOSError, PermissionOp};
use async_trait::async_trait;
use serde_json::Value;

use crate::hal::HalDriver;
use crate::types::{OpenFileEntry, OpenFilesResult};

pub struct OpenFilesDriver;

impl Default for OpenFilesDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl OpenFilesDriver {
    pub fn new() -> Self {
        Self
    }

    #[cfg(target_os = "linux")]
    pub fn list_open_files(&self, params: Value) -> Result<OpenFilesResult, AgentOSError> {
        use procfs::process::Process;

        let pid_filter = params.get("pid").and_then(|v| v.as_u64()).map(|v| v as i32);
        let path_filter = params
            .get("path_contains")
            .and_then(|v| v.as_str())
            .map(|s| s.to_lowercase());
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(200) as usize;
        // Default false: stop walking once `limit` rows are collected. Cheap on
        // large hosts. Set true to force a full walk for an accurate
        // `total_matched` count (slower).
        let accurate_total = params
            .get("accurate_total")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let mut files = Vec::new();
        let mut total_matched = 0usize;
        let mut incomplete = false;

        // Returns true if the caller should keep walking, false if it can stop.
        let collect_from_proc =
            |p: Process, files: &mut Vec<OpenFileEntry>, total: &mut usize| -> bool {
                let pid = p.pid() as u32;
                let name = p.stat().ok().map(|s| s.comm);
                let fds = match p.fd() {
                    // Per-process race tolerance: dead PID -> skip.
                    Ok(f) => f,
                    Err(_) => return true,
                };
                for fd_res in fds {
                    let fd = match fd_res {
                        Ok(f) => f,
                        Err(_) => continue,
                    };
                    let path = match &fd.target {
                        procfs::process::FDTarget::Path(p) => p.to_string_lossy().to_string(),
                        other => format!("{:?}", other),
                    };

                    if let Some(ref filter) = path_filter {
                        if !path.to_lowercase().contains(filter) {
                            continue;
                        }
                    }

                    *total += 1;
                    if files.len() < limit {
                        files.push(OpenFileEntry {
                            pid,
                            process_name: name.clone(),
                            fd: fd.fd,
                            path,
                            kind: match &fd.target {
                                procfs::process::FDTarget::Path(_) => "regular".to_string(),
                                procfs::process::FDTarget::Socket(_) => "socket".to_string(),
                                procfs::process::FDTarget::Pipe(_) => "pipe".to_string(),
                                procfs::process::FDTarget::AnonInode(_) => "anon_inode".to_string(),
                                _ => "other".to_string(),
                            },
                        });
                    }
                }
                true
            };

        if let Some(pid) = pid_filter {
            if let Ok(p) = Process::new(pid) {
                collect_from_proc(p, &mut files, &mut total_matched);
            }
        } else {
            match procfs::process::all_processes() {
                Ok(all_procs) => {
                    for p_res in all_procs {
                        let p = match p_res {
                            Ok(p) => p,
                            Err(_) => continue,
                        };
                        collect_from_proc(p, &mut files, &mut total_matched);
                        if !accurate_total && files.len() >= limit {
                            incomplete = true;
                            break;
                        }
                    }
                }
                Err(e) => {
                    tracing::debug!(
                        error = %e,
                        "system-open-files: procfs all_processes() failed"
                    );
                }
            }
        }

        let returned = files.len();
        Ok(OpenFilesResult {
            files,
            total_matched,
            returned,
            incomplete,
        })
    }

    #[cfg(not(target_os = "linux"))]
    pub fn list_open_files(&self, _params: Value) -> Result<OpenFilesResult, AgentOSError> {
        Err(AgentOSError::HalError(
            "system-open-files not supported on this platform".into(),
        ))
    }
}

#[async_trait]
impl HalDriver for OpenFilesDriver {
    fn name(&self) -> &str {
        "open_files"
    }

    fn required_permission(&self) -> (&str, PermissionOp) {
        ("system.open_files", PermissionOp::Read)
    }

    async fn query(&self, params: Value) -> Result<Value, AgentOSError> {
        let result = self.list_open_files(params)?;
        Ok(serde_json::to_value(result).map_err(|e| AgentOSError::HalError(e.to_string()))?)
    }
}

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_list_own_open_files() {
        let driver = OpenFilesDriver::new();
        let pid = std::process::id();
        let res = driver
            .list_open_files(serde_json::json!({ "pid": pid }))
            .unwrap();
        assert!(!res.files.is_empty());
        assert!(res.files.iter().all(|f| f.pid == pid as u32));
    }
}
