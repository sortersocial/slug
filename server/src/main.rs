use axum::{routing::get, Router};
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

use slugsocial_server::{
    api,
    event_log::EventLog,
    html,
    state::{AppConfig, AppState, KeyRecord},
};

fn env_var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|s| !s.trim().is_empty())
}

fn parse_keys(s: &str) -> Vec<KeyRecord> {
    // Format: "id1:key1,id2:key2" or "key1,key2" (id auto = first8 of key).
    s.split(',')
        .map(|part| part.trim())
        .filter(|p| !p.is_empty())
        .map(|p| {
            if let Some((id, secret)) = p.split_once(':') {
                KeyRecord {
                    id: id.trim().to_string(),
                    secret: secret.trim().to_string(),
                }
            } else {
                let secret = p.to_string();
                let id = secret.chars().take(8).collect::<String>();
                KeyRecord { id, secret }
            }
        })
        .collect()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse()?))
        .init();

    let data_dir = env_var("SLUG_DATA_DIR").unwrap_or_else(|| "/data".to_string());
    let event_log_path = env_var("SLUG_EVENT_LOG").unwrap_or_else(|| format!("{data_dir}/events.jsonl"));
    let keys_raw = env_var("SLUG_KEYS").unwrap_or_else(|| "dev:dev".to_string());
    let keys = parse_keys(&keys_raw);
    let rate_limit_per_minute: u32 = env_var("SLUG_RATE_LIMIT_PER_MIN")
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);

    let cfg = AppConfig {
        data_dir,
        event_log_path: event_log_path.clone(),
        keys,
        rate_limit_per_minute,
    };

    let state = AppState::new(cfg);

    // Load and reduce existing events.
    let (events, bad) = EventLog::new(event_log_path).load_all().await?;
    if !bad.is_empty() {
        tracing::warn!(bad_lines = bad.len(), "skipped corrupt JSONL lines");
    }
    {
        let mut reduced = state.reduced.write().await;
        for ev in events {
            reduced.apply_event(ev);
        }
    }

    let app = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/", get(html::index))
        .route("/t/:tag", get(html::tag_page))
        .route("/t/:tag/a/:aspect", get(html::tag_aspect_page))
        .route("/api/v0/vote", axum::routing::post(api::post_vote))
        .route("/api/v0/rank", get(api::get_rank))
        .with_state(state)
        .layer(TraceLayer::new_for_http());

    let port: u16 = env_var("PORT")
        .and_then(|s| s.parse().ok())
        .unwrap_or(8080);
    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    tracing::info!(%addr, "listening");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutdown");
}


