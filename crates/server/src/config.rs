//! What a deployment decides, as opposed to what the code decides.
//!
//! Carried as an `Extension` rather than folded into the router state, so the
//! state stays `SqlitePool` and every existing handler keeps its signature.
//! Tests build a config directly instead of setting process environment
//! variables, which two tests running in parallel cannot do safely.

use std::sync::Arc;

use crate::ratelimit::RateLimit;

/// Registrations allowed per client IP per hour.
const REGISTER_PER_HOUR: u32 = 5;
/// Ingest batches allowed per user per minute. The client flushes every five
/// seconds, so twelve is its steady-state rate; this is an order of magnitude
/// above that and still bounds a runaway tab.
const INGEST_PER_MINUTE: u32 = 120;

#[derive(Clone)]
pub struct ServerConfig {
    /// Whether `POST /v1/register` mints a token for anyone who asks.
    ///
    /// Off by default, and deliberately: this crate is the thing somebody
    /// clones and runs, and a server that hands out write credentials to the
    /// open internet is not a reasonable default for a local checkout. The
    /// public deployment opts in with `GRIDLINE_OPEN_REGISTRATION=1`.
    pub open_registration: bool,
    pub register_limit: Arc<RateLimit>,
    pub ingest_limit: Arc<RateLimit>,
}

impl ServerConfig {
    pub fn from_env() -> Self {
        Self {
            open_registration: flag("GRIDLINE_OPEN_REGISTRATION"),
            ..Self::closed()
        }
    }

    /// The default posture: everything on, registration off.
    pub fn closed() -> Self {
        Self {
            open_registration: false,
            register_limit: Arc::new(RateLimit::per_hour(REGISTER_PER_HOUR)),
            ingest_limit: Arc::new(RateLimit::per_minute(INGEST_PER_MINUTE)),
        }
    }

    /// A server that will register anyone. Used by the deployment and by the
    /// tests that exercise the endpoint.
    pub fn open() -> Self {
        Self {
            open_registration: true,
            ..Self::closed()
        }
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self::closed()
    }
}

fn flag(name: &str) -> bool {
    matches!(
        std::env::var(name).as_deref().map(str::trim),
        Ok("1") | Ok("true") | Ok("yes")
    )
}

/// Comma-separated origins the browser may call from, or `None` for the
/// permissive local-development default. Read here so `main` and the docs
/// agree on the name.
pub fn allowed_origins() -> Option<Vec<String>> {
    let raw = std::env::var("GRIDLINE_ALLOWED_ORIGINS").ok()?;
    let origins: Vec<String> = raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    // A variable set to blanks is a misconfiguration, not a request for an
    // empty allowlist that refuses every browser. Fall back and say so.
    (!origins.is_empty()).then_some(origins)
}

/// The address the rate limiter should charge for this request.
///
/// Behind a proxy the socket peer is the proxy, so every visitor would share
/// one bucket and the first five registrations of the hour would lock out the
/// world. `X-Forwarded-For` is `client, proxy1, proxy2, …`, appended
/// left-to-right, so the **rightmost** entry is the one our own proxy
/// observed. The leftmost is whatever the caller chose to send, which is why
/// taking it — the more common mistake — hands an attacker an unlimited
/// supply of fresh buckets.
///
/// This is only sound behind a proxy that appends. Directly exposed, there is
/// no `X-Forwarded-For` and the peer address is used instead.
pub fn client_key(headers: &axum::http::HeaderMap, peer: Option<std::net::IpAddr>) -> String {
    if let Some(xff) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
        if let Some(last) = xff.rsplit(',').map(str::trim).find(|s| !s.is_empty()) {
            return last.to_string();
        }
    }
    peer.map(|p| p.to_string())
        .unwrap_or_else(|| "unknown".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderMap;
    use std::net::{IpAddr, Ipv4Addr};

    fn xff(value: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", value.parse().unwrap());
        h
    }

    const PEER: IpAddr = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));

    #[test]
    fn the_proxy_appended_address_wins_over_the_one_the_client_sent() {
        // A caller spoofing a header gets the address our proxy saw charged
        // to them anyway.
        assert_eq!(
            client_key(&xff("1.2.3.4, 203.0.113.9"), Some(PEER)),
            "203.0.113.9"
        );
    }

    #[test]
    fn a_single_entry_is_used_as_is() {
        assert_eq!(client_key(&xff("203.0.113.9"), Some(PEER)), "203.0.113.9");
    }

    #[test]
    fn a_blank_or_absent_header_falls_back_to_the_socket_peer() {
        assert_eq!(client_key(&xff("  ,  "), Some(PEER)), "10.0.0.1");
        assert_eq!(client_key(&HeaderMap::new(), Some(PEER)), "10.0.0.1");
    }

    #[test]
    fn no_header_and_no_peer_still_yields_one_shared_bucket_rather_than_none() {
        // Sharing a bucket is a worse limit than per-IP but it is still a
        // limit; returning something unique per request would be no limit.
        assert_eq!(client_key(&HeaderMap::new(), None), "unknown");
    }
}
