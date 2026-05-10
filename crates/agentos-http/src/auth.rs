//! Auth header injection helpers for `reqwest::RequestBuilder`.
//!
//! These methods centralise the formatting of `Authorization` and API-key
//! headers so every adapter uses exactly the same format instead of each
//! constructing the string themselves.

pub trait AuthExt: Sized {
    /// Attach a `Bearer <token>` Authorization header.
    fn bearer(self, token: &str) -> Self;

    /// Attach an arbitrary named header, e.g. `X-Api-Key`.
    fn api_key_header(self, header_name: &str, key: &str) -> Self;
}

impl AuthExt for reqwest::RequestBuilder {
    fn bearer(self, token: &str) -> Self {
        self.header("authorization", format!("Bearer {token}"))
    }

    fn api_key_header(self, header_name: &str, key: &str) -> Self {
        self.header(header_name, key)
    }
}
