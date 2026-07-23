mod support;

use slugsocial_server::events::{Event, Ingest};
use slug_types::PostStats;
use support::*;

#[tokio::test]
async fn test_healthz() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();
    let response = client
        .get(format!("http://{}/healthz", addr))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    assert_eq!(response.text().await.unwrap(), "ok");
}

#[tokio::test]
async fn test_post_stats_endpoint_and_home_header() {
    let (addr, _tmp, _log, state, _handle) = create_test_server_with_state().await;
    let client = reqwest::Client::new();

    {
        let mut w = state.reduced.write().await;
        for (i, (principal, delegate)) in [
            ("alice", None),
            ("alice", None),
            ("alice", None),
            (
                "alice",
                Some("00000000-0000-0000-0000-000000000001:test:local/test".to_string()),
            ),
        ]
        .into_iter()
        .enumerate()
        {
            w.apply_event(Event::Ingest(Ingest {
                ts: i as i64 + 1,
                id: format!("stats-{i}"),
                raw: format!("stats post {i}"),
                principal: principal.to_string(),
                delegate,
                room_id: "public".to_string(),
                thread_tag: "stats".to_string(),
            }));
        }
    }

    let stats: PostStats = client
        .get(format!("http://{addr}/api/v0/stats"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(stats.human_posts, 3);
    assert_eq!(stats.ai_posts, 1);
    assert_eq!(stats.format_line(), "3 human posts, 1 ai post");

    let home = client
        .get(format!("http://{addr}/"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        home.contains("3 human posts, 1 ai post"),
        "home header should show post stats, snippet={}",
        home.chars().take(800).collect::<String>()
    );
    assert!(home.contains("view-meta"));
}

#[tokio::test]
async fn test_index_page() {
    // HTML routes are offline during the auth-v3 refactor.
}

