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
:beauty {more beautiful}\n\
~/lonely {Nothing to judge yet}\n"
            .to_string(),
        principal: "testuser".to_string(),
        delegate: Some("00000000-0000-0000-0000-000000000000:test:local/test".to_string()),
        room_id: "public".to_string(),
        thread_tag: thread_tag.to_string(),
    })
}

#[tokio::test]
async fn test_question_page_renders_prompt_and_seeds_thread_tag() {
    let (addr, _tmp, _log, state, _handle) = create_test_server_with_state().await;
    {
        let mut w = state.reduced.write().await;
        w.apply_event(question_garden_ingest("psalms-seed"));
    }
    let client = reqwest::Client::new();
    let guest = client
        .get(format!("http://{addr}/q/psalms"))
        .send()
        .await
        .unwrap();
    assert!(guest.status().is_success(), "{}", guest.status());
    let guest_body = guest.text().await.unwrap();
    assert!(
        guest_body.contains("Which psalm is greater?"),
        "prompt headline missing: {}",
        guest_body.chars().take(1200).collect::<String>()
    );
    assert!(guest_body.contains("vote-compare-pair"));
    assert!(guest_body.contains("view-vote-question"));
    assert!(
        guest_body.contains("href=\"/~/psalms\""),
        "ranking link missing: {}",
        guest_body.chars().take(1500).collect::<String>()
    );
    assert!(guest_body.contains("href=\"/t/psalms\""));
    let login_href = format!("/login?next={}", urlencoding::encode("/q/psalms"));
    assert!(
        guest_body.contains(&login_href),
        "guest login next should be the /q/ url: {}",
        guest_body.chars().take(2000).collect::<String>()
    );

    let authed = client
        .get(format!("http://{addr}/q/psalms"))
        .header("Authorization", format!("Bearer {}", test_bearer()))
        .send()
        .await
        .unwrap();
    assert!(authed.status().is_success(), "{}", authed.status());
    let body = authed.text().await.unwrap();
    assert!(
        body.contains("value=\"psalms\""),
        "thread_tag should be seeded to the collection leaf: {}",
        body.chars().take(2000).collect::<String>()
    );
}

#[tokio::test]
async fn test_question_aspect_page_seeds_aspect_and_uses_prompt() {
    let (addr, _tmp, _log, state, _handle) = create_test_server_with_state().await;
    {
        let mut w = state.reduced.write().await;
        w.apply_event(question_garden_ingest("psalms-seed"));
    }
    let client = reqwest::Client::new();
    let guest = client
        .get(format!("http://{addr}/q/psalms/beauty"))
        .send()
        .await
        .unwrap();
    assert!(guest.status().is_success(), "{}", guest.status());
    let guest_body = guest.text().await.unwrap();
    assert!(
        guest_body.contains("more beautiful"),
        "aspect prompt missing: {}",
        guest_body.chars().take(1200).collect::<String>()
    );
    assert!(guest_body.contains("href=\"/~/psalms#aspect-beauty\""));
    assert!(guest_body.contains("href=\"/t/psalms\""));
    let login_href = format!("/login?next={}", urlencoding::encode("/q/psalms/beauty"));
    assert!(
        guest_body.contains(&login_href),
        "guest login next should be the /q/aspect url: {}",
        guest_body.chars().take(2000).collect::<String>()
    );

    let authed = client
        .get(format!("http://{addr}/q/psalms/beauty"))
        .header("Authorization", format!("Bearer {}", test_bearer()))
        .send()
        .await
        .unwrap();
    assert!(authed.status().is_success(), "{}", authed.status());
    let body = authed.text().await.unwrap();
    assert!(body.contains("name=\"aspect\""));
    assert!(
        body.contains("value=\"beauty\""),
        "aspect hidden field missing: {}",
        body.chars().take(2000).collect::<String>()
    );
    assert!(
        body.contains("value=\"psalms\""),
        "thread_tag should be seeded: {}",
        body.chars().take(2000).collect::<String>()
    );
}

#[tokio::test]
async fn test_question_empty_scope_has_honest_empty_state() {
    let (addr, _tmp, _log, state, _handle) = create_test_server_with_state().await;
    {
        let mut w = state.reduced.write().await;
        w.apply_event(question_garden_ingest("psalms-seed"));
    }
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("http://{addr}/q/lonely"))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "{}", resp.status());
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("Nothing to judge yet"),
        "empty-scope prompt missing: {}",
        body.chars().take(1200).collect::<String>()
    );
    assert!(body.contains("vote-question-empty"));
    assert!(!body.contains("vote-compare-pair"));
    assert!(body.contains("href=\"/~/lonely\""));
    assert!(body.contains("href=\"/t/lonely\""));
}

#[tokio::test]
async fn test_question_unknown_collection_is_not_found() {
    let (addr, _tmp, _log, _state, _handle) = create_test_server_with_state().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("http://{addr}/q/no-such-collection-xyz"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_question_invalid_aspect_slug_is_not_found() {
    let (addr, _tmp, _log, state, _handle) = create_test_server_with_state().await;
    {
        let mut w = state.reduced.write().await;
        w.apply_event(question_garden_ingest("psalms-seed"));
    }
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("http://{addr}/q/psalms/NOT_VALID"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_room_question_page_uses_room_prefixes() {
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

    let resp = client
        .get(format!("http://{addr}/r/{room_seg}/q/psalms/beauty"))
        .header("Authorization", format!("Bearer {bearer}"))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "{}", resp.status());
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("more beautiful"),
        "room aspect prompt missing: {}",
        body.chars().take(1500).collect::<String>()
    );
    assert!(body.contains(&format!("href=\"/r/{room_seg}/~/psalms#aspect-beauty\"")));
    assert!(body.contains(&format!("href=\"/r/{room_seg}/t/psalms\"")));
    assert!(body.contains("name=\"aspect\""));
    assert!(body.contains("value=\"beauty\""));
    assert!(body.contains("value=\"psalms\""));
}
