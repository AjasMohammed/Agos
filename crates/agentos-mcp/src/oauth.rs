/// OAuth2 token provider abstraction for MCP HTTP transports.
///
/// Implementations are responsible for:
/// - Returning a current, valid access token on `get_token()`
/// - Refreshing the token when it is near expiry
/// - Performing an immediate refresh on `force_refresh()` (called after a 401)
///
/// The concrete implementation (`VaultOAuthProvider`) lives in `agentos-kernel`
/// where both `agentos-mcp` and `agentos-vault` are already in scope.
use async_trait::async_trait;

use crate::transport::McpTransportError;

/// Provides OAuth2 access tokens to the MCP HTTP transport.
#[async_trait]
pub trait OAuthTokenProvider: Send + Sync {
    /// Return the current access token, transparently refreshing if near expiry.
    async fn get_token(&self) -> Result<String, McpTransportError>;

    /// Force an immediate token refresh and return the new token.
    ///
    /// Called by the transport after receiving an HTTP 401 response, so that
    /// the request can be retried once with a fresh credential.
    async fn force_refresh(&self) -> Result<String, McpTransportError>;
}
