//! Gridline server binary: migrations, tracing, CORS, listener.
//!
//! `--seed-user [--admin]` mints a user and prints its token once, which is
//! the only moment the plaintext exists.

use axum::http::{header, HeaderValue, Method};
use sqlx::sqlite::SqlitePoolOptions;
use tower_http::{cors::CorsLayer, trace::TraceLayer};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("server=info,tower_http=info")),
        )
        .init();

    let database_url =
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite://gridline.db?mode=rwc".into());
    let pool = SqlitePoolOptions::new().connect(&database_url).await?;
    server::migrate(&pool).await?;

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--seed-user") {
        let user = server::seed_user(&pool, args.iter().any(|a| a == "--admin")).await?;
        println!("actor_id: {}", user.id);
        println!("token:    {}", user.token);
        println!("(shown once; the database stores only its SHA-256)");
        return Ok(());
    }

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8787);

    let config = server::ServerConfig::from_env();
    let open_registration = config.open_registration;
    let app = server::app_with(pool, config)
        .layer(cors())
        .layer(TraceLayer::new_for_http());

    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    // Both of these decide who can talk to this server, and getting either
    // wrong is silent — a permissive deployment looks exactly like a locked
    // down one until somebody looks. So they are stated at every start-up
    // rather than left to be inferred from the environment.
    tracing::info!(
        version = server::VERSION,
        addr = %listener.local_addr()?,
        open_registration,
        origins = ?server::config::allowed_origins()
            .map_or_else(|| "any (development default)".to_string(), |o| o.join(", ")),
        "gridline-server listening"
    );
    // `into_make_service_with_connect_info` rather than the plain one: the
    // rate limiter charges registrations to the caller's address, and without
    // this there is no address to charge when the server is exposed directly.
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;
    Ok(())
}

/// An allowlist when the deployment names one, permissive otherwise.
///
/// Permissive is right for local development — the Vite dev server is on
/// another port, so every request is cross-origin — and wrong for a public
/// deployment, where it invites any page on the internet to spend a visitor's
/// token. The default stays permissive so nobody's local loop breaks; the
/// deployment sets `GRIDLINE_ALLOWED_ORIGINS` and the start-up line above says
/// which of the two is in force.
fn cors() -> CorsLayer {
    let Some(origins) = server::config::allowed_origins() else {
        return CorsLayer::permissive();
    };
    let parsed: Vec<HeaderValue> = origins
        .iter()
        .filter_map(|o| match o.parse::<HeaderValue>() {
            Ok(v) => Some(v),
            Err(_) => {
                tracing::error!(origin = %o, "ignoring unparseable allowed origin");
                None
            }
        })
        .collect();
    if parsed.is_empty() {
        // Every entry was junk. Refusing every browser would be a confusing
        // way to fail, but so would silently going permissive, so: say it.
        tracing::error!("GRIDLINE_ALLOWED_ORIGINS had no usable entries; refusing all origins");
    }
    CorsLayer::new()
        .allow_origin(parsed)
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE])
}
