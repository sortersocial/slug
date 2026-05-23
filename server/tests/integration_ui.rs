mod support;

use slug_types::room_route_segment;
use support::*;

#[tokio::test]
async fn test_post_check_returns_targeted_js_error_for_missing_thread_tag() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let bearer = test_bearer();

    let rpc = ui_check_ingest_rpc("public", "", "hello", "thread-compose-errors");
    let resp = client
        .post(format!("http://{addr}/ui"))
        .header("Authorization", format!("Bearer {bearer}"))
        .form(&[("__rpc__", rpc.as_str())])
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/javascript; charset=utf-8")
    );
    let js = resp.text().await.unwrap();
    assert!(js.contains("#thread-compose-errors"));
    assert!(js.contains("missing thread tag"));
}

#[tokio::test]
async fn test_choose_username_returns_evalable_js() {
    let (addr, _tmp, _log, state, _handle) = create_test_server_with_state().await;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();

    let start = client
        .post(format!("http://{addr}/api/v0/pending-session"))
        .json(&serde_json::json!({
            "agent": "00000000-0000-0000-0000-000000000123:test:web/form"
        }))
        .send()
        .await
        .unwrap();
    assert!(start.status().is_success());
    let start_json: serde_json::Value = start.json().await.unwrap();
    let session = start_json["session"].as_str().unwrap();

    {
        let mut sessions = state.pending_sessions.write().await;
        let pending = sessions
            .get_mut(session)
            .expect("pending session must exist");
        pending.provider = Some("google".to_string());
        pending.provider_id = Some("google-user-123".to_string());
    }

    let choose = client
        .post(format!("http://{addr}/auth/choose-username"))
        .form(&[("session", session), ("username", "webuser")])
        .send()
        .await
        .unwrap();

    assert_eq!(choose.status(), reqwest::StatusCode::OK);
    assert_eq!(
        choose
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/javascript; charset=utf-8")
    );
    let body = choose.text().await.unwrap();
    assert!(body.contains("#choose-username-form"));
    assert!(body.contains("Idiomorph.morph"));
    assert!(body.contains("window.location = \"/auth/complete\""));
}

#[tokio::test]
async fn test_choose_username_carries_redirect_next() {
    let (addr, _tmp, _log, state, _handle) = create_test_server_with_state().await;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();

    let start = client
        .post(format!("http://{addr}/api/v0/pending-session"))
        .json(&serde_json::json!({
            "agent": "00000000-0000-0000-0000-000000000123:test:web/form"
        }))
        .send()
        .await
        .unwrap();
    assert!(start.status().is_success());
    let start_json: serde_json::Value = start.json().await.unwrap();
    let session = start_json["session"].as_str().unwrap();

    {
        let mut sessions = state.pending_sessions.write().await;
        let pending = sessions
            .get_mut(session)
            .expect("pending session must exist");
        pending.provider = Some("google".to_string());
        pending.provider_id = Some("google-user-redirect".to_string());
        pending.redirect_next = Some("/vote?left=%7E%2Fa&right=%7E%2Fb".to_string());
    }

    let choose = client
        .post(format!("http://{addr}/auth/choose-username"))
        .form(&[("session", session), ("username", "redirectuser")])
        .send()
        .await
        .unwrap();

    assert_eq!(choose.status(), reqwest::StatusCode::OK);
    let body = choose.text().await.unwrap();
    assert!(body.contains("window.location = \"/vote?left=%7E%2Fa&right=%7E%2Fb\""));
}

#[tokio::test]
async fn test_sse_stream_emits_evalable_js_after_post() {
    let (addr, _tmp, _log, _state, _handle) = create_test_server_with_state().await;
    let client = reqwest::Client::new();
    let bearer = test_bearer();

    let create = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "RoomCreate": { "slug": "sse-room" }
        }]),
    )
    .await;
    let room_id = create["results"][0]["result"]["RoomCreated"]["room_id"]
        .as_str()
        .unwrap()
        .to_string();
    let room_seg = room_route_segment(&room_id).unwrap();
    let room_path = format!("/r/{room_seg}");

    let sse_resp = client
        .get(format!(
            "http://{addr}/sse?path={}",
            urlencoding::encode(&room_path)
        ))
        .send()
        .await
        .unwrap();
    assert!(sse_resp.status().is_success());

    let rpc = ui_post_ingest_rpc(&room_id, "live-thread", "hello over sse");
    let _post = client
        .post(format!("http://{addr}/ui"))
        .header("Authorization", format!("Bearer {bearer}"))
        .form(&[("__rpc__", rpc.as_str())])
        .send()
        .await
        .unwrap();

    let mut body = String::new();
    let mut sse_resp = sse_resp;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(std::time::Duration::from_millis(500), sse_resp.chunk()).await {
            Ok(Ok(Some(chunk))) => {
                body.push_str(&String::from_utf8_lossy(&chunk));
                if body.contains("room-thread-feed") {
                    break;
                }
            }
            Ok(Ok(None)) => break,
            Ok(Err(err)) => panic!("sse read failed: {err}"),
            Err(_) => {}
        }
    }
    assert!(body.contains("room-thread-feed"));
    assert!(body.contains("live-thread"));
}

#[tokio::test]
async fn test_sse_public_thread_morph_includes_post_body_not_thread_not_found() {
    let (addr, _tmp, _log, _state, _handle) = create_test_server_with_state().await;
    let client = reqwest::Client::new();
    let bearer = test_bearer();

    let thread_tag = "sse-public-live";
    let seed_text = "seed public thread for sse";

    let rpc = ui_post_ingest_rpc("public", thread_tag, seed_text);
    let _post = client
        .post(format!("http://{addr}/ui"))
        .header("Authorization", format!("Bearer {bearer}"))
        .form(&[("__rpc__", rpc.as_str())])
        .send()
        .await
        .unwrap();

    let thread_path = format!("/t/{thread_tag}");
    let sse_resp = client
        .get(format!(
            "http://{addr}/sse?path={}",
            urlencoding::encode(&thread_path)
        ))
        .send()
        .await
        .unwrap();
    assert!(sse_resp.status().is_success());

    let reply_text = "hello over sse from another tab";
    let rpc2 = ui_post_ingest_rpc("public", thread_tag, reply_text);
    let _post2 = client
        .post(format!("http://{addr}/ui"))
        .header("Authorization", format!("Bearer {bearer}"))
        .form(&[("__rpc__", rpc2.as_str())])
        .send()
        .await
        .unwrap();

    let mut body = String::new();
    let mut sse_resp = sse_resp;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(std::time::Duration::from_millis(500), sse_resp.chunk()).await {
            Ok(Ok(Some(chunk))) => {
                body.push_str(&String::from_utf8_lossy(&chunk));
                if body.contains("thread-feed-region") && body.contains(reply_text) {
                    break;
                }
            }
            Ok(Ok(None)) => break,
            Ok(Err(err)) => panic!("sse read failed: {err}"),
            Err(_) => {}
        }
    }
    assert!(
        body.contains(reply_text),
        "sse morph should include new post text; got: {}",
        if body.len() > 2000 {
            format!("...{}", &body[body.len() - 2000..])
        } else {
            body.clone()
        }
    );
    assert!(
        !body.contains("thread not found"),
        "public thread morph must not use broken nav (thread not found)"
    );
}

