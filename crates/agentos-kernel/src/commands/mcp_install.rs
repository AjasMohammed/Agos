//! `agentos mcp install <id>` / `uninstall <id>` — one-command install of a
//! curated MCP server from the catalog.
//!
//! This is the headline UX win over the 6-step manual `mcp attach` chore: look
//! up the catalog entry, enforce the trust-tier policy, validate the runtime is
//! present (clear error if not), then **delegate to the existing
//! `cmd_mcp_attach`** — never re-implementing transport/spawn/handshake logic.
//!
//! v1 install model (one-shot, no prompt round-trips): `assume_yes` /
//! `allow_community` are passed up front; missing input surfaces as an
//! error-with-remediation rather than an interactive prompt inside the kernel.
//!
//! Strategy mapping for stdio transport: `npx` → `npx -y <package> <args…>`
//! (node); `pip` → `uvx <package> <args…>` (python, via uv's runner); `global`
//! → `<package-as-binary> <args…>`; `bundled` → `<package-as-path> <args…>`.
//! HTTP/SSE entries attach by their declared `url`. `vault:*` env refs are left
//! intact — `cmd_mcp_attach` resolves them against the vault at attach time.

use std::collections::HashMap;

use agentos_bus::KernelResponse;

use crate::kernel::Kernel;
use crate::mcp_catalog::CatalogEntry;

/// The `(command, args, env)` tuple the attach path consumes.
type InstallInvocation = (String, Vec<String>, HashMap<String, String>);

/// Expand the `{home}` placeholder in a catalog arg/value against `$HOME`.
fn expand_home(s: &str) -> String {
    match std::env::var_os("HOME") {
        Some(h) if s.contains("{home}") => s.replace("{home}", &h.to_string_lossy()),
        _ => s.to_string(),
    }
}

/// Pure mapping from a catalog entry to the `(command, args, env)` tuple the
/// attach path consumes. Performs no I/O or runtime resolution (the async
/// handler does that). Returns `Err(message)` for unsupported combinations.
///
/// `env` carries the auth env var → credential ref (e.g. `vault:github_token`),
/// which the attach path resolves; it is omitted when `no_auth` is set or the
/// entry declares no auth.
pub(crate) fn install_invocation(
    entry: &CatalogEntry,
    no_auth: bool,
) -> Result<InstallInvocation, String> {
    let args: Vec<String> = entry.install.args.iter().map(|a| expand_home(a)).collect();

    let require_package = |what: &str| {
        entry
            .install
            .package
            .clone()
            .ok_or_else(|| format!("entry '{}': {what} strategy requires a package", entry.id))
    };

    let (command, full_args) = match entry.install.strategy.as_str() {
        "npx" => {
            let pkg = require_package("npx")?;
            let mut a = vec!["-y".to_string(), pkg];
            a.extend(args);
            ("npx".to_string(), a)
        }
        "pip" => {
            // uvx runs the package's console script in an ephemeral env.
            let pkg = require_package("pip")?;
            let mut a = vec![pkg];
            a.extend(args);
            ("uvx".to_string(), a)
        }
        "global" => (require_package("global")?, args),
        "bundled" => (require_package("bundled")?, args),
        other => {
            return Err(format!(
                "entry '{}': install strategy '{other}' is not supported by `mcp install` \
                 — attach it manually with `agentos mcp attach`",
                entry.id
            ))
        }
    };

    let mut env = HashMap::new();
    if !no_auth && entry.auth.kind != "none" {
        if let (Some(var), Some(cred)) = (&entry.auth.env, &entry.auth.credential) {
            env.insert(var.clone(), cred.clone());
        }
    }

    Ok((command, full_args, env))
}

impl Kernel {
    /// Install an MCP server from the catalog: trust-gate → runtime-validate →
    /// delegate to `cmd_mcp_attach`.
    pub async fn cmd_mcp_install(
        &self,
        id: String,
        assume_yes: bool,
        allow_community: bool,
        runtime_binary_override: Option<String>,
        no_auth: bool,
    ) -> KernelResponse {
        let _ = assume_yes; // one-shot model: no interactive prompt in the kernel.

        let entry = match self.mcp_catalog.lookup(&id) {
            Some(e) => e.clone(),
            None => {
                return KernelResponse::Error {
                    message: format!(
                        "No catalog entry '{id}'. Try: agentos mcp catalog search <keyword>"
                    ),
                }
            }
        };

        // Trust-tier policy (fail-closed for community/blocked).
        match entry.trust_tier.as_str() {
            "community" if !allow_community => {
                return KernelResponse::Error {
                    message: format!(
                        "Catalog entry '{id}' is community-tier. Re-run with \
                         --unsafe-allow-community to install it."
                    ),
                }
            }
            "blocked" => {
                return KernelResponse::Error {
                    message: format!("Catalog entry '{id}' is blocked by policy."),
                }
            }
            _ => {}
        }

        // HTTP/SSE transports attach by declared URL (no local install step).
        if entry.mcp.transport != "stdio" {
            return match entry.mcp.url.clone() {
                Some(url) => {
                    self.cmd_mcp_attach(
                        id,
                        None,
                        vec![],
                        Some(url),
                        None,
                        None,
                        None,
                        HashMap::new(),
                    )
                    .await
                }
                None => KernelResponse::Error {
                    message: format!(
                        "Catalog entry '{id}' uses '{}' transport but declares no url.",
                        entry.mcp.transport
                    ),
                },
            };
        }

        let (mut command, args, mut env) = match install_invocation(&entry, no_auth) {
            Ok(t) => t,
            Err(message) => return KernelResponse::Error { message },
        };

        // Resolve the runtime binary the server must run under: an explicit
        // --runtime-binary wins (existence-checked, version check skipped);
        // otherwise the resolver picks the best installed runtime
        // (nvm/volta/asdf before system PATH).
        let runtime_binary = match &runtime_binary_override {
            Some(path) => {
                let p = std::path::PathBuf::from(path);
                if !p.is_file() {
                    return KernelResponse::Error {
                        message: format!("--runtime-binary {path} does not exist or is not a file"),
                    };
                }
                Some(p)
            }
            None => match &entry.install.runtime {
                Some(rt) => {
                    let min = entry.install.min_runtime_version.as_deref().unwrap_or("0");
                    match crate::runtime_resolver::resolve_by_name(rt, min) {
                        Ok(resolved) => Some(resolved.binary),
                        Err(e) => {
                            return KernelResponse::Error {
                                message: format!(
                                    "{e}. Install {rt} (>= {min}) or pass --runtime-binary <path>."
                                ),
                            }
                        }
                    }
                }
                None => None,
            },
        };

        // Pin the spawn to the resolved runtime instead of trusting the
        // kernel's PATH (the 2026-04-18 Gmail incident: `#!/usr/bin/env node`
        // resolved an ancient system node). The launcher colocated with the
        // runtime (nvm/volta/asdf ship npx next to node) becomes the absolute
        // command, and the runtime's bin dir is prepended to the child PATH so
        // env-shebang lookups inside the launcher resolve the same runtime.
        if let Some(bin_dir) = runtime_binary.as_deref().and_then(|b| b.parent()) {
            // join() with an absolute `command` (bundled/global strategies) is
            // a deliberate no-op; only relative launchers (npx/uvx) get pinned.
            let colocated = bin_dir.join(&command);
            if colocated.is_file() {
                command = colocated.to_string_lossy().into_owned();
            }
            let child_path = match std::env::var("PATH") {
                Ok(p) if !p.is_empty() => format!("{}:{p}", bin_dir.display()),
                _ => bin_dir.display().to_string(),
            };
            env.insert("PATH".to_string(), child_path);
        }

        tracing::info!(catalog_id = %id, command = %command, "Installing MCP server from catalog");
        self.cmd_mcp_attach(id, Some(command), args, None, None, None, None, env)
            .await
    }

    /// Uninstall a previously-installed catalog server. Attachments are
    /// persisted under the catalog `id` (used as the attach name), so this is a
    /// detach. `--purge` cache cleanup is a no-op in v1 because install
    /// delegates fetching to npx/uvx, which manage their own caches.
    pub async fn cmd_mcp_uninstall(&self, id: String, _purge: bool) -> KernelResponse {
        self.cmd_mcp_detach(id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_catalog::CatalogRegistry;

    fn seed(id: &str) -> CatalogEntry {
        CatalogRegistry::load(None)
            .unwrap()
            .lookup(id)
            .cloned()
            .unwrap_or_else(|| panic!("seed {id} missing"))
    }

    #[test]
    fn npx_entry_builds_npx_invocation() {
        let (cmd, args, env) = install_invocation(&seed("filesystem"), false).unwrap();
        assert_eq!(cmd, "npx");
        assert_eq!(args[0], "-y");
        assert_eq!(args[1], "@modelcontextprotocol/server-filesystem");
        // -y, package, and the (expanded) directory arg.
        assert_eq!(args.len(), 3);
        // No auth → empty env.
        assert!(env.is_empty());
    }

    #[test]
    fn api_key_entry_injects_credential_env() {
        let (cmd, _args, env) = install_invocation(&seed("github"), false).unwrap();
        assert_eq!(cmd, "npx");
        assert_eq!(
            env.get("GITHUB_PERSONAL_ACCESS_TOKEN").map(String::as_str),
            Some("vault:github_token")
        );
    }

    #[test]
    fn no_auth_flag_suppresses_credential_env() {
        let (_cmd, _args, env) = install_invocation(&seed("github"), true).unwrap();
        assert!(env.is_empty());
    }

    #[test]
    fn pip_entry_builds_uvx_invocation() {
        let (cmd, args, _env) = install_invocation(&seed("sqlite"), false).unwrap();
        assert_eq!(cmd, "uvx");
        assert_eq!(args[0], "mcp-server-sqlite");
        assert!(args.iter().any(|a| a == "--db-path"));
    }
}
