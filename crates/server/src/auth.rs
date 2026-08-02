//! Bearer-token auth. The database stores `sha256(token)` and nothing else,
//! so a dump of `users` cannot be replayed against the API.

use axum::extract::{FromRef, FromRequestParts};
use axum::http::{header::AUTHORIZATION, request::Parts};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use ulid::Ulid;

use crate::{error::ApiError, now_rfc3339};

/// An authenticated caller.
#[derive(Debug, Clone)]
pub struct AuthUser {
    pub id: String,
    pub is_admin: bool,
}

/// An authenticated caller that is also an admin; 403 otherwise.
#[derive(Debug, Clone)]
pub struct AdminUser(pub AuthUser);

pub fn token_hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

fn bearer(parts: &Parts) -> Option<&str> {
    let value = parts.headers.get(AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then(|| token.trim())
        .filter(|t| !t.is_empty())
}

impl<S> FromRequestParts<S> for AuthUser
where
    SqlitePool: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let pool = SqlitePool::from_ref(state);
        let hash = token_hash(bearer(parts).ok_or(ApiError::Unauthorized)?);
        let row: Option<(String, i64)> =
            sqlx::query_as("SELECT id, is_admin FROM users WHERE token_hash = ?")
                .bind(&hash)
                .fetch_optional(&pool)
                .await?;
        let (id, is_admin) = row.ok_or(ApiError::Unauthorized)?;
        Ok(AuthUser {
            id,
            is_admin: is_admin != 0,
        })
    }
}

impl<S> FromRequestParts<S> for AdminUser
where
    SqlitePool: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let user = AuthUser::from_request_parts(parts, state).await?;
        if !user.is_admin {
            return Err(ApiError::Forbidden("admin token required".into()));
        }
        Ok(AdminUser(user))
    }
}

/// A user created for development or tests. The token is returned once and
/// is not recoverable afterwards.
#[derive(Debug, Clone)]
pub struct SeededUser {
    pub id: String,
    pub token: String,
}

pub async fn seed_user(pool: &SqlitePool, is_admin: bool) -> Result<SeededUser, sqlx::Error> {
    let id = format!("u_{}", Ulid::new().to_string().to_lowercase());
    // Two ULIDs: 160 bits of CSPRNG randomness plus a timestamp, which is
    // plenty for a locally issued token that only ever exists as a hash here.
    let token = format!("gl_{}{}", Ulid::new(), Ulid::new());
    sqlx::query("INSERT INTO users (id, token_hash, is_admin, created_at) VALUES (?, ?, ?, ?)")
        .bind(&id)
        .bind(token_hash(&token))
        .bind(i64::from(is_admin))
        .bind(now_rfc3339())
        .execute(pool)
        .await?;
    Ok(SeededUser { id, token })
}
