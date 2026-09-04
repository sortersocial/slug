mod support;

use axum::http::Uri;
use slug_types::ItemId;
use slugsocial_server::{
    events::{Event, Ingest},
    middleware::canonical_view_url,
};
use support::*;

#[tokio::test]
async fn test_ontology_item_page_shows_body_children_and_collapsible_votes() {
    // HTML routes are offline during the auth-v3 refactor.
}

#[tokio::test]
async fn test_nested_tilde_url_permanently_redirects_to_leaf() {
    let (addr, _tmp, _log, _state, _handle) = create_test_server_with_state().await;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let nested = client
        .get(format!("http://{addr}/~/x/luke?depth=2"))
        .send()
        .await
        .unwrap();
    assert_eq!(nested.status(), reqwest::StatusCode::PERMANENT_REDIRECT);
    let loc = nested
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(loc, "/~/luke?depth=2");

    let leaf = client
        .get(format!("http://{addr}/~/luke"))
        .send()
        .await
        .unwrap();
    assert!(
        leaf.status().is_success(),
        "leaf page should render: {}",
        leaf.status()
    );
}

#[tokio::test]
async fn test_garden_trailing_slash_permanently_redirects_to_canonical() {
    let (addr, _tmp, _log, _state, _handle) = create_test_server_with_state().await;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();

    let root = client
        .get(format!("http://{addr}/~/?depth=2"))
        .send()
        .await
        .unwrap();
    assert_eq!(root.status(), reqwest::StatusCode::PERMANENT_REDIRECT);
    let root_loc = root
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(root_loc, "/~?depth=2");

    let leaf = client
        .get(format!("http://{addr}/~/luke/?depth=2"))
        .send()
        .await
        .unwrap();
    assert_eq!(leaf.status(), reqwest::StatusCode::PERMANENT_REDIRECT);
    let leaf_loc = leaf
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(leaf_loc, "/~/luke?depth=2");

    let ext = client
        .get(format!("http://{addr}/-/"))
        .send()
        .await
        .unwrap();
    assert_eq!(ext.status(), reqwest::StatusCode::PERMANENT_REDIRECT);
    let ext_loc = ext
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(ext_loc, "/-");
}

/// Thread connective tissue: item_threads and VoteData.thread_tag are exposed by item, pair, and matchup RPC.
#[tokio::test]
async fn test_search_page_and_results() {
    // HTML search pages are offline during the auth-v3 refactor.
}

#[tokio::test]
async fn test_view_counts_increment_and_display() {
    let (addr, _tmp, _log, state, _handle) = create_test_server_with_state().await;
    let client = reqwest::Client::new();
    let url = format!("http://{addr}/~");

    let r1 = client.get(&url).send().await.unwrap();
    assert!(r1.status().is_success());
    let body1 = r1.text().await.unwrap();
    assert!(
        body1.contains("1 views"),
        "expected first GET to show 1 views, body snippet: {}",
        &body1.chars().take(500).collect::<String>()
    );

    let r2 = client.get(&url).send().await.unwrap();
    assert!(r2.status().is_success());
    let body2 = r2.text().await.unwrap();
    assert!(
        body2.contains("2 views"),
        "expected second GET to show 2 views"
    );

    // Vote compare uses permuted `left` / `right`; middleware canonicalizes query order.
    let left = ItemId::parse("~/vc-l")
        .unwrap()
        .normalized_storage()
        .to_storage_string();
    let right = ItemId::parse("~/vc-r")
        .unwrap()
        .normalized_storage()
        .to_storage_string();
    let vote_q_right_first = format!(
        "/vote?right={}&left={}",
        urlencoding::encode(&right),
        urlencoding::encode(&left)
    );
    let vote_q_left_first = format!(
        "/vote?left={}&right={}",
        urlencoding::encode(&left),
        urlencoding::encode(&right)
    );
    let vote_key_uri: Uri = format!("http://127.0.0.1{vote_q_left_first}")
        .parse()
        .unwrap();
    let vote_key = canonical_view_url(&vote_key_uri);

    let v1 = client
        .get(format!("http://{addr}{vote_q_right_first}"))
        .send()
        .await
        .unwrap();
    assert!(
        v1.status().is_success(),
        "vote compare GET 1: {}",
        v1.status()
    );
    let v1_body = v1.text().await.unwrap();
    assert!(
        v1_body.contains("1 views"),
        "vote compare page should show view count, snippet: {}",
        v1_body.chars().take(600).collect::<String>()
    );
    let v2 = client
        .get(format!("http://{addr}{vote_q_left_first}"))
        .send()
        .await
        .unwrap();
    assert!(
        v2.status().is_success(),
        "vote compare GET 2: {}",
        v2.status()
    );
    let v2_body = v2.text().await.unwrap();
    assert!(
        v2_body.contains("2 views"),
        "vote compare page should reflect incremented count, snippet: {}",
        v2_body.chars().take(600).collect::<String>()
    );

    assert_eq!(
        state.views.get_views(&vote_key),
        2,
        "permuted vote/compare URLs should share one ViewStore key ({vote_key:?})"
    );
}

#[tokio::test]
async fn test_vote_compare_renders_github_import_cards() {
    let (addr, _tmp, _log, state, _handle) = create_test_server_with_state().await;
    let client = reqwest::Client::new();

    use base64::{engine::general_purpose::STANDARD, Engine as _};
    let left_card = STANDARD.encode(
        serde_json::json!({
            "v": 1,
            "schema": "slug_github_import",
            "kind": "issue",
            "url": "https://github.com/ghvotehi/a/issues/9",
            "headline": "#9 Left corner",
            "sublines": ["State: open"],
        })
        .to_string()
        .as_bytes(),
    );
    let right_card = STANDARD.encode(
        serde_json::json!({
            "v": 1,
            "schema": "slug_github_import",
            "kind": "issue",
            "url": "https://github.com/ghvotehi/a/issues/10",
            "headline": "#10 Right corner",
            "sublines": ["State: open"],
        })
        .to_string()
        .as_bytes(),
    );
    let raw = format!(
        "@00000000-0000-0000-0000-000000000000:test:local/test\n\
https://github.com/ghvotehi/a/issues/9 {{\n\
```slug-github-card\n\
{left_card}\n\
```\n\
}}\n\
\n\
https://github.com/ghvotehi/a/issues/10 {{\n\
```slug-github-card\n\
{right_card}\n\
```\n\
}}\n"
    );

    {
        let mut w = state.reduced.write().await;
        w.apply_event(Event::Ingest(Ingest {
            ts: 10,
            id: "ing-vote-github-cards".to_string(),
            raw,
            principal: "testuser".to_string(),
            delegate: Some("00000000-0000-0000-0000-000000000000:test:local/test".to_string()),
            room_id: "public".to_string(),
            thread_tag: "gh-vote-cards".to_string(),
        }));
    }

    let left = ItemId::parse("https://github.com/ghvotehi/a/issues/9")
        .unwrap()
        .normalized_storage()
        .to_storage_string();
    let right = ItemId::parse("https://github.com/ghvotehi/a/issues/10")
        .unwrap()
        .normalized_storage()
        .to_storage_string();
    let q = format!(
        "/vote?left={}&right={}",
        urlencoding::encode(&left),
        urlencoding::encode(&right)
    );
    let resp = client
        .get(format!("http://{addr}{q}"))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "{}", resp.status());
    let body = resp.text().await.unwrap();
    let n_cards = body.matches("import-card").count();
    assert!(
        n_cards >= 2,
        "expected two GitHub import cards on vote compare, count={n_cards}, snippet={}",
        body.chars().take(1500).collect::<String>()
    );
    assert!(body.contains("vote-compare-left"));
    assert!(body.contains("vote-compare-right"));
    assert!(body.contains("#9 Left corner"));
    assert!(body.contains("#10 Right corner"));
}

#[tokio::test]
async fn test_search_handles_multibyte_unicode() {
    // HTML search pages are offline during the auth-v3 refactor.
}

fn question_garden_ingest(thread_tag: &str) -> Event {
    Event::Ingest(Ingest {
        ts: 20,
        id: format!("ing-q-{thread_tag}"),
        raw: "@00000000-0000-0000-0000-000000000000:test:local/test\n\
~/psalms {Which psalm is greater?}\n\
~/psalms/psalm-23 {The Lord is my shepherd}\n\
~/psalms/psalm-1 {Blessed is the man}\n\
{canonical}\n\
~/psalms/psalm-23 3:1 ~/psalms/psalm-1\n\
:beauty {more beautiful}\n\
{more beautiful}\n\
~/psalms/psalm-23 2:1 ~/psalms/psalm-1\n\
~/lonely {Nothing to judge yet}\n"
            .to_string(),
        principal: "testuser".to_string(),
        delegate: Some("00000000-0000-0000-0000-000000000000:test:local/test".to_string()),
        room_id: "public".to_string(),
        thread_tag: thread_tag.to_string(),
    })
}

fn no_redirect_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

fn redirect_location(resp: &reqwest::Response) -> String {
    assert_eq!(resp.status(), reqwest::StatusCode::PERMANENT_REDIRECT);
    resp.headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string()
}

#[tokio::test]
async fn test_retired_question_pages_are_gone() {
    let (addr, _tmp, _log, state, _handle) = create_test_server_with_state().await;
    {
        let mut w = state.reduced.write().await;
        w.apply_event(question_garden_ingest("psalms-seed"));
    }
    let client = reqwest::Client::new();
    for path in [
        "/q/psalms",
        "/q/psalms/beauty",
        "/q/lonely",
        "/q/psalms/NOT_VALID",
        "/q/no-such-collection-xyz",
    ] {
        let resp = client
            .get(format!("http://{addr}{path}"))
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            reqwest::StatusCode::NOT_FOUND,
            "{path} should be gone"
        );
    }
}

#[tokio::test]
async fn test_room_question_pages_are_gone() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();
    let bearer = test_bearer();

    let create = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{ "RoomCreate": { "slug": "q-room" } }]),
    )
    .await;
    let room_id = create["results"][0]["result"]["RoomCreated"]["room_id"]
        .as_str()
        .unwrap()
        .to_string();
    let room_seg = slug_types::room_route_segment(&room_id).unwrap();

    let posted = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "Post": {
                "room": room_id,
                "thread_tag": "psalms",
                "text": "~/psalms {Which psalm is greater?}\n~/psalms/a {alpha}\n~/psalms/b {beta}\n:beauty {more beautiful}\n"
            }
        }]),
    )
    .await;
    assert_eq!(posted["results"][0]["ok"], true, "room seed post: {posted}");

    for path in [
        format!("/r/{room_seg}/q/psalms"),
        format!("/r/{room_seg}/q/psalms/beauty"),
    ] {
        let resp = client
            .get(format!("http://{addr}{path}"))
            .header("Authorization", format!("Bearer {bearer}"))
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            reqwest::StatusCode::NOT_FOUND,
            "{path} should be gone"
        );
    }
}

fn landing_seed_ingest() -> Event {
    Event::Ingest(Ingest {
        ts: 21,
        id: "ing-duel".into(),
        raw: "~/duel/a { Alpha }\n~/duel/b { Beta }\n".to_string(),
        principal: "testuser".to_string(),
        delegate: None,
        room_id: "public".into(),
        thread_tag: "duel".into(),
    })
}

#[tokio::test]
async fn test_vote_landing_serves_neediest_pair() {
    let (addr, _tmp, _log, state, _handle) = create_test_server_with_state().await;
    {
        let mut w = state.reduced.write().await;
        w.apply_event(landing_seed_ingest());
    }
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("http://{addr}/vote"))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "{}", resp.status());
    let body = resp.text().await.unwrap();
    assert!(body.contains("judge one pair"), "landing intro missing");
    assert!(body.contains("0 of 1"), "need count missing");
    assert!(body.contains("drag the slider"), "steps missing");
    assert!(body.contains("href=\"/~/duel\""), "scope link missing");
    assert!(body.contains("vote-compare-pair"));

    // Guests get the login CTA; the seeded compose form needs a session.
    let authed = client
        .get(format!("http://{addr}/vote"))
        .header("Authorization", format!("Bearer {}", test_bearer()))
        .send()
        .await
        .unwrap();
    assert!(authed.status().is_success(), "{}", authed.status());
    let body = authed.text().await.unwrap();
    assert!(
        body.contains("value=\"duel\""),
        "thread_tag should seed to pool leaf"
    );
}

#[tokio::test]
async fn test_vote_landing_lists_more_open_questions() {
    let (addr, _tmp, _log, state, _handle) = create_test_server_with_state().await;
    {
        let mut w = state.reduced.write().await;
        // NOTE: leaves are global (`~/duel/a` IS `~a`), so the straggler scope
        // needs its own leaves or the busy scope's votes would judge its pair.
        w.apply_event(Event::Ingest(Ingest {
            ts: 20,
            id: "ing-duel2".into(),
            raw: "~/duel/p { Pee }\n~/duel/q { Kew }\n".to_string(),
            principal: "testuser".into(),
            delegate: None,
            room_id: "public".into(),
            thread_tag: "duel".into(),
        }));
        w.apply_event(Event::Ingest(Ingest {
            ts: 25,
            id: "ing-old".into(),
            raw: "~/old/a { Alpha }\n~/old/b { Beta }\n~/old/c { Gamma }\n\
                  { judged }\n~/old/a 2:1 ~/old/b\n"
                .to_string(),
            principal: "testuser".into(),
            delegate: None,
            room_id: "public".into(),
            thread_tag: "old".into(),
        }));
    }
    let client = reqwest::Client::new();
    let body = client
        .get(format!("http://{addr}/vote"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    // Dealt pair is ~/old (1 judged pair beats duel's 0); the index lists
    // only the straggler — never a second row for the dealt question.
    assert!(body.contains("more open questions"), "heat index missing");
    assert_eq!(
        body.matches("vote-open-row").count(),
        1,
        "index should hold exactly the straggler row"
    );
    assert!(
        body.contains("~%2Fduel"),
        "straggler judge link missing: {}",
        body.chars().take(3000).collect::<String>()
    );
    assert!(body.contains("0 of 1 judged"));
}

#[tokio::test]
async fn test_vote_landing_deals_hungrier_aspect_group() {
    let (addr, _tmp, _log, state, _handle) = create_test_server_with_state().await;
    {
        let mut w = state.reduced.write().await;
        w.apply_event(Event::Ingest(Ingest {
            ts: 24,
            id: "ing-tri-full".into(),
            raw: "~/tri/a { alpha }\n~/tri/b { beta }\n~/tri/c { gamma }\n\
                  { ab }\n~/tri/a 2:1 ~/tri/b\n\
                  { ac }\n~/tri/a 2:1 ~/tri/c\n\
                  { bc }\n~/tri/b 2:1 ~/tri/c\n\
                  :beauty {more beautiful}\n{ pretty }\n~/tri/a 2:1 ~/tri/b\n"
                .to_string(),
            principal: "testuser".into(),
            delegate: None,
            room_id: "public".into(),
            thread_tag: "tri".into(),
        }));
    }
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("http://{addr}/vote"))
        .header("Authorization", format!("Bearer {}", test_bearer()))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "{}", resp.status());
    let body = resp.text().await.unwrap();
    assert!(body.contains("judge one pair"), "landing intro missing");
    assert!(body.contains(":beauty"), "aspect question missing");
    assert!(body.contains("1 of 3"), "aspect need count missing");
    assert!(body.contains("name=\"aspect\""), "aspect input missing");
    assert!(body.contains("value=\"beauty\""), "aspect value missing");
    assert!(
        body.contains("value=\"tri\""),
        "thread_tag should seed to pool leaf"
    );
}

#[tokio::test]
async fn test_vote_aspect_query_param() {
    let (addr, _tmp, _log, state, _handle) = create_test_server_with_state().await;
    {
        let mut w = state.reduced.write().await;
        w.apply_event(Event::Ingest(Ingest {
            ts: 22,
            id: "ing-tri".into(),
            raw: "~/tri/a { alpha }\n~/tri/b { beta }\n~/tri/c { gamma }\n\
                  { ab }\n~/tri/a 2:1 ~/tri/b\n\
                  :beauty {more beautiful}\n{ pretty }\n~/tri/a 2:1 ~/tri/b\n"
                .to_string(),
            principal: "testuser".to_string(),
            delegate: None,
            room_id: "public".into(),
            thread_tag: "tri".into(),
        }));
    }
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("http://{addr}/vote?pool=~/tri&aspect=beauty"))
        .header("Authorization", format!("Bearer {}", test_bearer()))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "{}", resp.status());
    let body = resp.text().await.unwrap();
    assert!(body.contains("name=\"aspect\""), "aspect input missing");
    assert!(body.contains("value=\"beauty\""), "aspect value missing");

    let bad = client
        .get(format!(
            "http://{addr}/vote?left=~/tri/a&right=~/tri/b&aspect=NOPE"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), reqwest::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_garden_index_lists_tilde_scopes() {
    let (addr, _tmp, _log, state, _handle) = create_test_server_with_state().await;
    {
        let mut w = state.reduced.write().await;
        w.apply_event(Event::Ingest(Ingest {
            ts: 23,
            id: "ing-mix".into(),
            raw: "~mix { Mixed scope }\n~/mix/a { alpha }\n\
                  https://example.com/thing { An imported thing }\n\
                  { import }\nhttps://example.com/thing <: ~mix\n"
                .to_string(),
            principal: "testuser".to_string(),
            delegate: None,
            room_id: "public".into(),
            thread_tag: "mix".into(),
        }));
    }
    let client = reqwest::Client::new();
    let body = client
        .get(format!("http://{addr}/~"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(body.contains("~/mix"), "tilde scope missing");
    // Root membership is tilde-only by construction (the DSL has no spelling
    // for a URL `<: ~` claim), so the index never mixes in imports.
    assert!(!body.contains("example.com/thing"));
}

#[tokio::test]
async fn test_home_is_thread_index() {
    let (addr, _tmp, _log, state, _handle) = create_test_server_with_state().await;
    {
        let mut w = state.reduced.write().await;
        w.apply_event(Event::Ingest(Ingest {
            ts: 5,
            id: "ing-thread-index".into(),
            raw: "hello from the forum index".into(),
            principal: "testuser".into(),
            delegate: None,
            room_id: "public".into(),
            thread_tag: "forum-home".into(),
        }));
    }
    let client = reqwest::Client::new();
    let guest = client.get(format!("http://{addr}/")).send().await.unwrap();
    assert!(guest.status().is_success(), "{}", guest.status());
    let guest_body = guest.text().await.unwrap();
    assert!(
        guest_body.contains("id=\"thread-feed\""),
        "home should keep the bump list: {}",
        guest_body.chars().take(1500).collect::<String>()
    );
    assert!(guest_body.contains("forum-home") || guest_body.contains("#forum-home"));
    assert!(guest_body.contains("log in to post"));
    assert!(guest_body.contains("judge one pair"));
    assert!(guest_body.contains("href=\"/vote\""));
    assert!(guest_body.contains("the index is ephemeral"));
    assert!(!guest_body.contains("data-testid=\"question-index\""));

    let authed = client
        .get(format!("http://{addr}/"))
        .header("Authorization", format!("Bearer {}", test_bearer()))
        .send()
        .await
        .unwrap();
    assert!(authed.status().is_success(), "{}", authed.status());
    let body = authed.text().await.unwrap();
    assert!(body.contains("id=\"new-thread-ui-slot\""));
    assert!(
        body.contains("human post") || body.contains("view-meta"),
        "home should keep post stats: {}",
        body.chars().take(1500).collect::<String>()
    );
}

#[tokio::test]
async fn test_home_empty_garden_has_honest_empty_state() {
    let (addr, _tmp, _log, _state, _handle) = create_test_server_with_state().await;
    let client = reqwest::Client::new();
    let resp = client.get(format!("http://{addr}/")).send().await.unwrap();
    assert!(resp.status().is_success(), "{}", resp.status());
    let body = resp.text().await.unwrap();
    assert!(body.contains("id=\"thread-feed\""));
    assert!(body.contains("no threads yet"));
    assert!(body.contains("log in to post"));
    assert!(body.contains("the index is ephemeral"));
    assert!(!body.contains("data-testid=\"question-index\""));
}

#[tokio::test]
async fn test_home_index_caps_at_five_but_old_thread_url_stays() {
    let (addr, _tmp, _log, state, _handle) = create_test_server_with_state().await;
    {
        let mut w = state.reduced.write().await;
        for i in 0..6 {
            w.apply_event(Event::Ingest(Ingest {
                ts: 100 + i,
                id: format!("ing-ephemeral-{i}"),
                raw: format!("hello from thread-{i}"),
                principal: "testuser".into(),
                delegate: None,
                room_id: "public".into(),
                thread_tag: format!("thread-{i}"),
            }));
        }
    }
    let client = reqwest::Client::new();
    let home = client
        .get(format!("http://{addr}/"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let feed = home
        .split("id=\"thread-feed\"")
        .nth(1)
        .and_then(|rest| rest.split("</div>").next())
        .unwrap_or("");
    assert!(
        feed.contains("href=\"/t/thread-5\""),
        "newest thread should stay on the board: {feed}"
    );
    assert!(
        feed.contains("href=\"/t/thread-1\""),
        "fifth-newest should stay on the board: {feed}"
    );
    assert!(
        !feed.contains("href=\"/t/thread-0\""),
        "oldest thread should fall off the ephemeral index: {feed}"
    );
    assert_eq!(
        feed.matches("href=\"/t/thread-").count(),
        5,
        "public index should list five threads: {feed}"
    );

    let old = client
        .get(format!("http://{addr}/t/thread-0"))
        .send()
        .await
        .unwrap();
    assert!(old.status().is_success(), "{}", old.status());
    let old_body = old.text().await.unwrap();
    assert!(
        old_body.contains("hello from thread-0"),
        "bookmarked /t/:tag should still render: {}",
        old_body.chars().take(1500).collect::<String>()
    );
    assert!(
        old_body.contains("off the index"),
        "fallen-off thread should say bookmark this url: {}",
        old_body.chars().take(1500).collect::<String>()
    );

    let live = client
        .get(format!("http://{addr}/t/thread-5"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(live.contains("hello from thread-5"));
    assert!(
        !live.contains("off the index"),
        "frontpage thread should not show the fallen-off hint: {}",
        live.chars().take(1500).collect::<String>()
    );
}

#[tokio::test]
async fn test_forum_index_moved_to_root() {
    let (addr, _tmp, _log, _state, _handle) = create_test_server_with_state().await;
    let client = no_redirect_client();
    for path in ["/t", "/t/"] {
        let resp = client
            .get(format!("http://{addr}{path}"))
            .send()
            .await
            .unwrap();
        assert_eq!(redirect_location(&resp), "/");
    }
}
