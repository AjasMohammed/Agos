use serde::Deserialize;
use zeroize::Zeroizing;

/// No `Debug`/`Clone`/`Serialize`: `Zeroizing` has no `Clone`, a derived `Debug`
/// would print the plaintext secret into any log line that formats the request,
/// and `Serialize` would let `serde_json::to_string` re-emit it just as easily.
#[derive(Deserialize, utoipa::ToSchema)]
pub struct SetSecretRequest {
    pub name: String,
    /// Zeroized on drop; wire type is a plain JSON string.
    #[schema(value_type = String)]
    pub value: Zeroizing<String>,
    /// `global` | `kernel` | `agent:<name>` | `tool:<name>`. REQUIRED — there is
    /// deliberately no default, because `global` makes the secret readable by
    /// every agent and tool on the host (same rule as the CLI's `--scope`).
    pub scope: String,
}
