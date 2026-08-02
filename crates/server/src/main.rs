//! Gridline server binary: migrations, tracing, CORS, listener.
//!
//! `--seed-user [--admin]` mints a user and prints its token once, which is
//! the only moment the plaintext exists.

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
    let app = server::app(pool)
        // Permissive CORS is for local development only: the web app is
        // served from a Vite dev server on another port.
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http());

    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    tracing::info!(
        version = server::VERSION,
        addr = %listener.local_addr()?,
        "gridline-server listening"
    );
    axum::serve(listener, app).await?;
    Ok(())
}
