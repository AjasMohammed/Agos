//! Bearer-token auth middleware for the REST API.
//!
//! Extracts `Authorization: Bearer agos_<key>` from incoming requests and
//! validates the key against the [`ApiKeyStore`]. Public routes (like `/v1/health`)
//! are excluded by mounting them outside the middleware layer.

use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Extension;

use crate::api_key::{ApiKeyRecord, ApiKeyStore};

/// Validated API key record, injected as a request extension after auth passes.
#[derive(Clone, Debug)]
pub struct AuthenticatedKey(pub ApiKeyRecord);

/// Axum middleware that enforces Bearer token authentication.
///
/// Reads `Authorization: Bearer <key>` from the request headers, validates
/// the key via the [`ApiKeyStore`], and injects an [`AuthenticatedKey`]
/// extension on success. Returns 401 Unauthorized on failure.
pub async fn require_api_key(
    Extension(store): Extension<ApiKeyStore>,
    mut request: Request,
    next: Next,
) -> Response {
    let auth_header = request
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let key = match auth_header {
        Some(ref h) if h.starts_with("Bearer ") => &h[7..],
        _ => {
            return (
                StatusCode::UNAUTHORIZED,
                axum::Json(serde_json::json!({
                    "error": {
                        "code": "UNAUTHORIZED",
                        "message": "Missing or invalid Authorization header. Expected: Bearer agos_<key>",
                        "status": 401
                    }
                })),
            )
                .into_response();
        }
    };

    match store.validate(key).await {
        Some(record) => {
            request.extensions_mut().insert(AuthenticatedKey(record));
            next.run(request).await
        }
        None => (
            StatusCode::UNAUTHORIZED,
            axum::Json(serde_json::json!({
                "error": {
                    "code": "UNAUTHORIZED",
                    "message": "Invalid or expired API key",
                    "status": 401
                }
            })),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api_key::ApiKeyStore;
    use axum::body::Body;
    use axum::http::Request;
    use axum::middleware;
    use axum::routing::get;
    use axum::Router;
    use tower::ServiceExt;

    async fn ok_handler() -> &'static str {
        "ok"
    }

    fn test_app(store: ApiKeyStore) -> Router {
        Router::new()
            .route("/protected", get(ok_handler))
            .layer(middleware::from_fn(require_api_key))
            .layer(Extension(store))
    }

    #[tokio::test]
    async fn missing_auth_header_returns_401() {
        let store = ApiKeyStore::with_secret(vec![1u8; 32]);
        let app = test_app(store);

        let req = Request::builder()
            .uri("/protected")
            .body(Body::empty())
            .expect("request");

        let resp = app.oneshot(req).await.expect("response");

        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn valid_key_passes() {
        let store = ApiKeyStore::with_secret(vec![1u8; 32]);
        let key = store.create_key("test".into(), vec![], None).await;
        let app = test_app(store);

        let req = Request::builder()
            .uri("/protected")
            .header("Authorization", format!("Bearer {key}"))
            .body(Body::empty())
            .expect("request");

        let resp = app.oneshot(req).await.expect("response");

        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn invalid_key_returns_401() {
        let store = ApiKeyStore::with_secret(vec![1u8; 32]);
        let _key = store.create_key("test".into(), vec![], None).await;
        let app = test_app(store);

        let req = Request::builder()
            .uri("/protected")
            .header(
                "Authorization",
                "Bearer agos_badbadbadbadbadbadbadbadbadbadbadbadbadbadbadbadbadbadbadbadbadbad",
            )
            .body(Body::empty())
            .expect("request");

        let resp = app.oneshot(req).await.expect("response");

        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
}
