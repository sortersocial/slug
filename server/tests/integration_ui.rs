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
async fn test_post_check_rejects_thread_tag_with_slash() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let bearer = test_bearer();

    let rpc = ui_check_ingest_rpc("public", "foo/bar", "hello", "thread-compose-errors");
    let resp = client
        .post(format!("http://{addr}/ui"))
        .header("Authorization", format!("Bearer {bearer}"))
        .form(&[("__rpc__", rpc.as_str())])
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let js = resp.text().await.unwrap();
    assert!(js.contains("#thread-compose-errors"));
    assert!(
        js.contains("must not contain"),
        "expected slash rejection morph, got: {js}"
    );
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
        .header("Authorization", format!("Bearer {bearer}"))
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
async fn test_sse_private_room_requires_valid_room_path_or_auth() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();

    // Segment too short to decode as `short/slug` → reject subscription (no JS execution).
    let bad_seg = client
        .get(format!(
            "http://{addr}/sse?path={}",
            urlencoding::encode("/r/abcdefg")
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(bad_seg.status(), reqwest::StatusCode::FORBIDDEN);

    let room_ok = rpc_batch(
        &client,
        addr,
        Some(&test_bearer()),
        serde_json::json!([{ "RoomCreate": { "slug": "sse-auth-room" } }]),
    )
    .await;
    let room_id = room_ok["results"][0]["result"]["RoomCreated"]["room_id"]
        .as_str()
        .unwrap();
    let seg = room_route_segment(room_id).unwrap();
    let room_path = format!("/r/{seg}");

    let no_cookie = client
        .get(format!(
            "http://{addr}/sse?path={}",
            urlencoding::encode(&room_path)
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(no_cookie.status(), reqwest::StatusCode::FORBIDDEN);

    let with_auth = client
        .get(format!(
            "http://{addr}/sse?path={}",
            urlencoding::encode(&room_path)
        ))
        .header("Authorization", format!("Bearer {}", test_bearer()))
        .send()
        .await
        .unwrap();
    assert!(with_auth.status().is_success());
}

#[tokio::test]
async fn test_sse_private_feed_not_sent_to_home_without_membership() {
    let (addr, _tmp, _log, _state, _handle) = create_test_server_with_state().await;

    let client = reqwest::Client::new();
    let owner = test_bearer();

    let create = rpc_batch(
        &client,
        addr,
        Some(&owner),
        serde_json::json!([{ "RoomCreate": { "slug": "sse-acl-home" } }]),
    )
    .await;
    let room_id = create["results"][0]["result"]["RoomCreated"]["room_id"]
        .as_str()
        .unwrap()
        .to_string();

    let sse_resp = client
        .get(format!(
            "http://{addr}/sse?path={}",
            urlencoding::encode("/")
        ))
        .send()
        .await
        .unwrap();
    assert!(sse_resp.status().is_success());

    let secret_phrase = "PRIVATE_SSE_HOME_LEAK_TEST_UNIQUE";
    let rpc = ui_post_ingest_rpc(&room_id, "acl-thread", secret_phrase);
    let _post = client
        .post(format!("http://{addr}/ui"))
        .header("Authorization", format!("Bearer {owner}"))
        .form(&[("__rpc__", rpc.as_str())])
        .send()
        .await
        .unwrap();

    let mut body = String::new();
    let mut sse_resp = sse_resp;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(std::time::Duration::from_millis(400), sse_resp.chunk()).await {
            Ok(Ok(Some(chunk))) => {
                body.push_str(&String::from_utf8_lossy(&chunk));
            }
            Ok(Ok(None)) => break,
            Ok(Err(err)) => panic!("sse read failed: {err}"),
            Err(_) => {}
        }
    }
    assert!(
        !body.contains(secret_phrase),
        "anonymous home SSE must not receive private room morph payload"
    );
}

#[tokio::test]
async fn test_sse_private_feed_masked_for_non_member_even_with_bearer() {
    let (addr, _tmp, _log, state, _handle) = create_test_server_with_state().await;
    let other = seed_test_identity(&state, "otheruser", "othertok", "othersecret").await;

    let client = reqwest::Client::new();
    let owner = test_bearer();

    let create = rpc_batch(
        &client,
        addr,
        Some(&owner),
        serde_json::json!([{ "RoomCreate": { "slug": "sse-acl-other" } }]),
    )
    .await;
    let room_id = create["results"][0]["result"]["RoomCreated"]["room_id"]
        .as_str()
        .unwrap()
        .to_string();

    let sse_resp = client
        .get(format!(
            "http://{addr}/sse?path={}",
            urlencoding::encode("/")
        ))
        .header("Authorization", format!("Bearer {other}"))
        .send()
        .await
        .unwrap();
    assert!(sse_resp.status().is_success());

    let secret_phrase = "PRIVATE_SSE_OTHER_USER_LEAK";
    let rpc = ui_post_ingest_rpc(&room_id, "leak-thread", secret_phrase);
    let _post = client
        .post(format!("http://{addr}/ui"))
        .header("Authorization", format!("Bearer {owner}"))
        .form(&[("__rpc__", rpc.as_str())])
        .send()
        .await
        .unwrap();

    let mut body = String::new();
    let mut sse_resp = sse_resp;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(std::time::Duration::from_millis(400), sse_resp.chunk()).await {
            Ok(Ok(Some(chunk))) => {
                body.push_str(&String::from_utf8_lossy(&chunk));
            }
            Ok(Ok(None)) => break,
            Ok(Err(err)) => panic!("sse read failed: {err}"),
            Err(_) => {}
        }
    }
    assert!(
        !body.contains(secret_phrase),
        "non-member must not receive private room SSE payloads even when subscribed from `/` with a bearer"
    );
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

/// The SSE thread-region push must be page-scoped: guarded morphs for the latest
/// page (append target) and the page before it, never an unguarded rewrite that
/// would overwrite whatever `?offset=` page a viewer selected.
#[tokio::test]
async fn test_sse_thread_morph_is_page_scoped_after_rollover() {
    let (addr, _tmp, _log, _state, _handle) = create_test_server_with_state().await;
    let client = reqwest::Client::new();
    let bearer = test_bearer();

    let thread_tag = "sse-page-scope";
    // 11 posts → posts 0..=9 fill page offset 0, post 10 starts the latest page (offset 10).
    for i in 0..11 {
        let commands = serde_json::json!([{
            "Post": { "room": "public", "thread_tag": thread_tag, "text": format!("seed post {i}") }
        }]);
        let resp = rpc_batch(&client, addr, Some(&bearer), commands).await;
        assert_eq!(resp["results"][0]["ok"], serde_json::json!(true));
    }

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

    let reply_text = "post eleven lands on the latest page";
    let rpc = ui_post_ingest_rpc("public", thread_tag, reply_text);
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
                if body.contains(reply_text) {
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
        "latest-page morph should include new post text"
    );
    assert!(
        body.contains("__slugPageOff >= 10"),
        "thread morph must be guarded to the latest page (offset 10); got: {}",
        &body[..body.len().min(2000)]
    );
    assert!(
        body.contains("__slugPageOff === 0"),
        "previous page (offset 0) should get a guarded refresh so its paginator sees the newer page"
    );
    assert!(
        body.contains("seed post 0"),
        "previous-page refresh should re-render page-0 posts"
    );
    // The new post must only appear inside the latest-page morph, after its guard —
    // never before the first page guard (which would be an unguarded rewrite).
    let first_guard = body.find("__slugPageOff").unwrap();
    let reply_at = body.find(reply_text).unwrap();
    assert!(
        reply_at > first_guard,
        "new post markup must be inside a page-offset guard"
    );
}

/// Arbitrary `?offset=` values snap to fixed page windows so post positions are
/// stable as a thread grows.
#[tokio::test]
async fn test_thread_view_offset_snaps_to_page_boundaries() {
    let (addr, _tmp, _log, _state, _handle) = create_test_server_with_state().await;
    let client = reqwest::Client::new();
    let bearer = test_bearer();

    let thread_tag = "page-snap";
    for i in 0..11 {
        let commands = serde_json::json!([{
            "Post": { "room": "public", "thread_tag": thread_tag, "text": format!("chrono post {i}") }
        }]);
        let resp = rpc_batch(&client, addr, Some(&bearer), commands).await;
        assert_eq!(resp["results"][0]["ok"], serde_json::json!(true));
    }

    // Default page = oldest window 0..10.
    let first = client
        .get(format!("http://{addr}/t/{thread_tag}"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(first.contains("1–10 / 11"), "first page shows fixed window");
    assert!(first.contains("chrono post 0"));
    assert!(!first.contains("chrono post 10"));

    // Mid-page offset snaps down to the containing page.
    let snapped = client
        .get(format!("http://{addr}/t/{thread_tag}?offset=7"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        snapped.contains("1–10 / 11"),
        "offset=7 snaps to page starting at 0"
    );

    // Latest page is the short, page-aligned tail window.
    let latest = client
        .get(format!("http://{addr}/t/{thread_tag}?offset=10"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(latest.contains("11–11 / 11"), "latest page is offset 10");
    assert!(latest.contains("chrono post 10"));
    assert!(!latest.contains("chrono post 9"));

    // Offsets past the end clamp to the latest page.
    let clamped = client
        .get(format!("http://{addr}/t/{thread_tag}?offset=999"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(clamped.contains("11–11 / 11"), "huge offset clamps to latest");
}

fn ui_vote_compare_post_rpc(
    room: &str,
    thread_tag: &str,
    left: &str,
    right: &str,
    ratio_left: &str,
    ratio_right: &str,
    explanation: &str,
) -> String {
    ui_vote_compare_post_rpc_with_next(
        room,
        thread_tag,
        left,
        right,
        ratio_left,
        ratio_right,
        explanation,
        "/vote",
    )
}

fn ui_vote_compare_post_rpc_with_next(
    room: &str,
    thread_tag: &str,
    left: &str,
    right: &str,
    ratio_left: &str,
    ratio_right: &str,
    explanation: &str,
    next: &str,
) -> String {
    serde_json::json!({
        "action": "vote_compare_post",
        "room": room,
        "thread_tag": thread_tag,
        "left_item": left,
        "right_item": right,
        "ratio_left": ratio_left,
        "ratio_right": ratio_right,
        "explanation": explanation,
        "next": next,
        "form_action": "/ui",
    })
    .to_string()
}

#[tokio::test]
async fn test_vote_compare_guest_page_shows_form_with_login_next() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();

    let left = urlencoding::encode("~/guest-vote-a");
    let right = urlencoding::encode("~/guest-vote-b");
    let pair_path = format!("/vote?left={left}&right={right}");
    let resp = client
        .get(format!("http://{addr}{pair_path}"))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "{}", resp.status());
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("id=\"vote-compare-form\""),
        "guests must see the vote compose UI on a shared pair link"
    );
    assert!(
        body.contains("vote-compare-login-cta"),
        "guests must see the post vote login CTA"
    );
    let login_href = format!("/login?next={}", urlencoding::encode(&pair_path));
    assert!(
        body.contains(&login_href),
        "guest post vote CTA must be /login?next=<pair>; snippet={}",
        body.chars().take(2500).collect::<String>()
    );
}

#[tokio::test]
async fn test_vote_compare_guest_post_redirects_to_login_with_pair_next() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();

    let next = "/vote?left=%7E%2Fguest-a&right=%7E%2Fguest-b";
    let rpc = ui_vote_compare_post_rpc_with_next(
        "public",
        "guest-vote",
        "~/guest-a",
        "~/guest-b",
        "50",
        "50",
        "prefer left",
        next,
    );
    let resp = client
        .post(format!("http://{addr}/ui"))
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
    let expected_login = format!(
        "window.location = \"/login?next={}\";",
        urlencoding::encode(next)
    );
    assert_eq!(
        js, expected_login,
        "unauthenticated vote post must JS-redirect to login with pair next"
    );
}

#[tokio::test]
async fn test_web_login_carries_vote_pair_next_into_pending_session() {
    let (addr, _tmp, _log, state, _handle) = create_test_server_with_state().await;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();

    let next = "/vote?left=%7E%2Fa&right=%7E%2Fb";
    let resp = client
        .get(format!(
            "http://{addr}/login?next={}",
            urlencoding::encode(next)
        ))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), reqwest::StatusCode::TEMPORARY_REDIRECT);
    let loc = resp
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .expect("Location header");
    assert!(
        loc.contains("/auth/login?session="),
        "web login should start OAuth pending session, loc={loc}"
    );
    assert!(
        loc.contains(&format!("next={}", urlencoding::encode(next)))
            || loc.contains("next=%2Fvote"),
        "auth/login redirect must carry vote pair next, loc={loc}"
    );

    // Pending session in RAM must remember the pair for post-OAuth redirect.
    let session = loc
        .split("session=")
        .nth(1)
        .and_then(|s| s.split('&').next())
        .map(|s| urlencoding::decode(s).unwrap_or_default().into_owned())
        .expect("session id in Location");
    let sessions = state.pending_sessions.read().await;
    let pending = sessions.get(&session).expect("pending session");
    assert_eq!(pending.redirect_next.as_deref(), Some(next));
    assert_eq!(
        pending.agent, None,
        "browser /login must not invent a sentinel delegate"
    );
}

#[tokio::test]
async fn test_vote_compare_two_users_both_succeed_without_delegate() {
    let (addr, _tmp, _log, state, _handle) = create_test_server_with_state().await;
    let client = reqwest::Client::new();
    let alice = test_bearer();
    let bob = seed_test_identity(&state, "bob", "bobtok", "bobsecret").await;

    // Define items first (votes require existing item bodies).
    let seed = ui_post_ingest_rpc(
        "public",
        "multi-vote",
        "~/multi-a {alpha}\n~/multi-b {beta}\n",
    );
    let seed_resp = client
        .post(format!("http://{addr}/ui"))
        .header("Authorization", format!("Bearer {alice}"))
        .form(&[("__rpc__", seed.as_str())])
        .send()
        .await
        .unwrap();
    assert_eq!(seed_resp.status(), reqwest::StatusCode::OK);
    let seed_js = seed_resp.text().await.unwrap();
    assert!(
        !seed_js.contains("auth-error"),
        "item seed must succeed, got: {seed_js}"
    );

    for (bearer, left, right, explanation) in [
        (&alice, "3", "1", "alice prefers a"),
        (&bob, "1", "3", "bob prefers b"),
    ] {
        let rpc = ui_vote_compare_post_rpc(
            "public",
            "multi-vote",
            "~/multi-a",
            "~/multi-b",
            left,
            right,
            explanation,
        );
        let resp = client
            .post(format!("http://{addr}/ui"))
            .header("Authorization", format!("Bearer {bearer}"))
            .form(&[("__rpc__", rpc.as_str())])
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let js = resp.text().await.unwrap();
        assert!(
            !js.contains("delegate already bound"),
            "human vote must not hit shared-sentinel AgentBound ({explanation}), got: {js}"
        );
        assert!(
            !js.contains("auth-error"),
            "human vote must succeed ({explanation}), got: {js}"
        );
        assert!(
            js.contains("vote-edge-history-region"),
            "vote should morph edge history ({explanation}), got: {js}"
        );
    }

    let reduced = state.reduced.read().await;
    let human_votes: Vec<_> = reduced
        .ingests_ordered
        .iter()
        .filter_map(|id| reduced.ingests_by_id.get(id))
        .filter(|ing| ing.raw.contains("prefers"))
        .collect();
    assert_eq!(human_votes.len(), 2, "expected two vote ingests");
    let mut principals: Vec<&str> = human_votes.iter().map(|i| i.principal.as_str()).collect();
    principals.sort();
    assert_eq!(principals, ["bob", "testuser"]);
    for ing in &human_votes {
        assert!(
            ing.delegate.is_none(),
            "browser votes must have no delegate, principal={} delegate={:?}",
            ing.principal,
            ing.delegate
        );
    }
    assert!(
        reduced.agent_bindings.is_empty(),
        "human votes must not create AgentBound entries: {:?}",
        reduced.agent_bindings
    );
}

#[tokio::test]
async fn test_vote_compare_post_rejects_zero_left_ratio() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();
    let bearer = test_bearer();

    let rpc = ui_vote_compare_post_rpc("public", "test-vote", "~/a", "~/b", "0", "5", "prefer b");
    let resp = client
        .post(format!("http://{addr}/ui"))
        .header("Authorization", format!("Bearer {bearer}"))
        .form(&[("__rpc__", rpc.as_str())])
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let js = resp.text().await.unwrap();
    assert!(
        js.contains("invalid ratio") || js.contains("≥ 1"),
        "expected zero-ratio rejection, got: {js}"
    );
}

#[tokio::test]
async fn test_vote_compare_post_rejects_zero_right_ratio() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();
    let bearer = test_bearer();

    let rpc = ui_vote_compare_post_rpc("public", "test-vote", "~/a", "~/b", "5", "0", "prefer a");
    let resp = client
        .post(format!("http://{addr}/ui"))
        .header("Authorization", format!("Bearer {bearer}"))
        .form(&[("__rpc__", rpc.as_str())])
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let js = resp.text().await.unwrap();
    assert!(
        js.contains("invalid ratio") || js.contains("≥ 1"),
        "expected zero-ratio rejection, got: {js}"
    );
}

#[tokio::test]
async fn test_vote_compare_post_rejects_over_max_ratio() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();
    let bearer = test_bearer();

    let rpc =
        ui_vote_compare_post_rpc("public", "test-vote", "~/a", "~/b", "101", "1", "prefer a");
    let resp = client
        .post(format!("http://{addr}/ui"))
        .header("Authorization", format!("Bearer {bearer}"))
        .form(&[("__rpc__", rpc.as_str())])
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let js = resp.text().await.unwrap();
    assert!(
        js.contains("invalid ratio") || js.contains("≤ 100"),
        "expected over-max ratio rejection, got: {js}"
    );
}

#[tokio::test]
async fn test_copy_garden_rank_returns_clipboard_js_with_markdown() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();
    let bearer = test_bearer();

    let seed = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "Post": {
                "room": "public",
                "thread_tag": "copy-rank-ui",
                "text": "# copy-rank-ui\n\n~/copy-a {a}\n~/copy-b {b}\n~/copy-c {c}\n{vote}\n~/copy-a 2:1 ~/copy-b\n",
                "return_rank_diff": false
            }
        }]),
    )
    .await;
    assert_eq!(seed["results"][0]["ok"], true, "seed: {:?}", seed);

    let page = client
        .get(format!("http://{addr}/~"))
        .send()
        .await
        .unwrap();
    assert!(page.status().is_success());
    let html = page.text().await.unwrap();
    assert!(
        html.contains("id=\"garden-rank-copy\"") && html.contains("copy_garden_rank"),
        "garden index should include copy button + action payload"
    );

    let rpc = serde_json::json!({
        "action": "copy_garden_rank",
        "room": "public",
        "parent_path": "~/",
        "depth": 1,
        "copy_btn_id": "garden-rank-copy",
    })
    .to_string();
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
    assert!(
        js.contains("navigator.clipboard.writeText") && js.contains("garden-rank-copy"),
        "expected clipboard JsBuilder snippet, got: {js}"
    );
    assert!(
        js.contains("1. ~/copy-a") && js.contains("2. ~/copy-b") && js.contains("- ~/copy-c"),
        "expected concise markdown ranking in clipboard payload, got: {js}"
    );
    assert!(js.contains("\"copied\""), "expected button label flip to copied");
}

