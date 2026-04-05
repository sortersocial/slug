use axum::Router;
use tracing_subscriber::EnvFilter;

use slugsocial_server::{
    event_log::EventLog,
    create_app,
    state::{AppConfig, AppState},
};

fn env_var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|s| !s.trim().is_empty())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Intentionally use stderr so we always see this in platform logs,
    // even if tracing is misconfigured.
    eprintln!("[boot] slugsocial-server starting");

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse()?))
        .init();

    let data_dir = env_var("SLUG_DATA_DIR").unwrap_or_else(|| "/data".to_string());
    let event_log_path = env_var("SLUG_EVENT_LOG").unwrap_or_else(|| format!("{data_dir}/events.jsonl"));

    eprintln!("[boot] data_dir={data_dir} event_log_path={event_log_path}");

    let cfg = AppConfig {
        data_dir: data_dir.clone(),
        event_log_path: event_log_path.clone(),
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

    // Single source of truth for routing lives in the library (`create_app`).
    let app: Router = create_app(state);

    let port: u16 = env_var("PORT")
        .and_then(|s| s.parse().ok())
        .unwrap_or(8080);
    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    tracing::info!(%addr, "listening");
    eprintln!("[boot] binding {addr}");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    eprintln!("[boot] server exited");
    Ok(())
}

async fn shutdown_signal() {
    // Fly and other platforms typically use SIGTERM. Ctrl+C is SIGINT.
    // We log the exact signal that caused shutdown to help diagnose early exits.
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm =
            signal(SignalKind::terminate()).expect("failed to install SIGTERM handler");
        let mut sigint = signal(SignalKind::interrupt()).expect("failed to install SIGINT handler");

        tokio::select! {
            _ = sigterm.recv() => {
                eprintln!("[shutdown] received SIGTERM");
                tracing::info!("shutdown: sigterm");
            }
            _ = sigint.recv() => {
                eprintln!("[shutdown] received SIGINT");
                tracing::info!("shutdown: sigint");
            }
        }
    }

    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        eprintln!("[shutdown] ctrl_c");
        tracing::info!("shutdown");
    }
}


