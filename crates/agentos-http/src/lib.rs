pub mod auth;
pub use auth::AuthExt;

use std::time::Duration;

/// Canonical HTTP client profiles.
///
/// Each profile encodes the right timeout / redirect settings for a class of
/// outbound connection.  Pick the closest profile instead of calling
/// `reqwest::Client::builder()` directly so timeouts are consistent across the
/// whole workspace.
pub enum HttpProfile {
    /// Generic outbound API call: 10 s connect, 30 s total, follow ≤ 5 redirects.
    Outbound,
    /// LLM provider streaming: 30 s connect, 300 s total, follow ≤ 3 redirects.
    Llm,
    /// Webhook delivery: 10 s total, no redirect follow.
    Webhook,
    /// Kernel-internal traffic: 5 s total, no redirects.
    Internal,
}

/// Build a `reqwest::Client` pre-configured for `profile`.
///
/// All clients share:
/// * `User-Agent: agentos/0.1.0`
/// * connection pool idle timeout of 90 s
///
/// # Panics
/// Panics if the static configuration is invalid (should never happen in
/// normal usage; the builder only fails on bad TLS config or OS errors).
pub fn client(profile: HttpProfile) -> reqwest::Client {
    let builder = reqwest::Client::builder()
        .user_agent("agentos/0.1.0")
        .pool_idle_timeout(Duration::from_secs(90));

    let builder = match profile {
        HttpProfile::Outbound => builder
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::limited(5)),
        HttpProfile::Llm => builder
            .connect_timeout(Duration::from_secs(30))
            .timeout(Duration::from_secs(300))
            .redirect(reqwest::redirect::Policy::limited(3)),
        HttpProfile::Webhook => builder
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none()),
        HttpProfile::Internal => builder
            .timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none()),
    };

    builder.build().expect("static HTTP client config is valid")
}
