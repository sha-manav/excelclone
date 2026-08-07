//! `POST /v1/register` — an anonymous account for a visitor who has one.
//!
//! There is no email, no password and no profile: the account is an opaque id
//! and a token, which is the same identity model `--seed-user` already mints,
//! reached over HTTP instead of over a shell. Nothing here grants consent.
//! A freshly registered user has no consent record, so the client shows the
//! notice and captures nothing until it is answered — registering is being
//! able to speak to the server, not agreeing to send it anything.

use axum::{
    extract::{ConnectInfo, State},
    http::HeaderMap,
    Extension, Json,
};
use serde::Serialize;
use sqlx::SqlitePool;
use std::net::SocketAddr;

use crate::{
    auth::seed_user,
    config::{client_key, ServerConfig},
    error::ApiError,
};

#[derive(Debug, Serialize)]
pub struct RegisterResponse {
    /// Shown once. The database keeps only its SHA-256.
    pub token: String,
    /// The identity the server will expect on every envelope from now on.
    pub actor_id: String,
}

pub async fn register(
    State(pool): State<SqlitePool>,
    Extension(cfg): Extension<ServerConfig>,
    // `ConnectInfo` is stored as a request extension, and reaching it that way
    // rather than through its own extractor keeps it optional: the tests drive
    // the router with `oneshot`, which has no socket behind it, and
    // `Option<ConnectInfo<_>>` is not a shape axum accepts.
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
) -> Result<Json<RegisterResponse>, ApiError> {
    if !cfg.open_registration {
        // Said plainly, because the alternative is a client that retries a
        // 404 forever and a user watching a chip that never turns green.
        return Err(ApiError::Forbidden(
            "this server does not accept new registrations".into(),
        ));
    }

    let key = client_key(&headers, peer.map(|Extension(ConnectInfo(addr))| addr.ip()));
    if !cfg.register_limit.allow(&key) {
        return Err(ApiError::TooManyRequests(
            "too many registrations from this address".into(),
        ));
    }

    let user = seed_user(&pool, false).await?;
    tracing::info!(actor_id = %user.id, "registered an anonymous user");
    Ok(Json(RegisterResponse {
        token: user.token,
        actor_id: user.id,
    }))
}
