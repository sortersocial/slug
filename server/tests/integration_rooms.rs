mod support;

use slug_types::room_route_segment;
use support::*;

#[tokio::test]
async fn test_room_create_rpc() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();
    let batch = serde_json::json!([{
        "RoomCreate": { "slug": "secret-project" }
    }]);
    let body = rpc_batch(&client, addr, Some(&test_bearer()), batch).await;
    let line = &body["results"][0];
    assert_eq!(line["ok"], true, "room create: {:?}", line);
    let room_id = line["result"]["RoomCreated"]["room_id"].as_str().unwrap();
    assert!(
        room_id.contains("/secret-project"),
        "expected room_id to contain slug, got {room_id}"
    );
}

#[tokio::test]
async fn test_room_delete_rpc() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();
    let bearer = test_bearer();

    let create = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "RoomCreate": { "slug": "rpc-delete-me" }
        }]),
    )
    .await;
    let room_id = create["results"][0]["result"]["RoomCreated"]["room_id"]
        .as_str()
        .unwrap()
        .to_string();

    let del = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "RoomDelete": { "room": room_id }
        }]),
    )
    .await;
    assert_eq!(del["results"][0]["ok"], true, "{:?}", del["results"][0]);
    assert!(
        del["results"][0]["result"]["RoomDeletedOk"].is_object(),
        "expected RoomDeletedOk: {:?}",
        del["results"][0]
    );

    let list = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!(["RoomList"]),
    )
    .await;
    let rooms = list["results"][0]["result"]["RoomList"]["rooms"]
        .as_array()
        .unwrap();
    assert!(
        !rooms.iter().any(|r| r.as_str() == Some(room_id.as_str())),
        "deleted room should not appear in RoomList"
    );

    let thread = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "GetForumThread": {
                "room": room_id,
                "thread_tag": "x",
                "offset": null,
                "limit": null,
                "since": null,
                "before": null,
                "actor": null,
                "post_id": null
            }
        }]),
    )
    .await;
    assert_eq!(thread["results"][0]["ok"], false);
    assert_eq!(thread["results"][0]["error"], "room not found");
}

/// Private room reads use `authorize_room_read`, which returns "room not found" when no bearer is sent
/// (same as unknown room). The CLI must attach the saved token for `private … forum show` etc.
#[tokio::test]
async fn test_private_room_forum_read_requires_bearer() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();
    let bearer = test_bearer();

    let create = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "RoomCreate": { "slug": "bearer-for-reads" }
        }]),
    )
    .await;
    let room_id = create["results"][0]["result"]["RoomCreated"]["room_id"]
        .as_str()
        .unwrap()
        .to_string();

    let post = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "Post": {
                "room": room_id,
                "thread_tag": "cli-read-test",
                "delegate": "00000000-0000-0000-0000-000000000000:test:local/test",
                "text": "hello from private room\n",
                "return_rank_diff": false
            }
        }]),
    )
    .await;
    assert_eq!(post["results"][0]["ok"], true);

    let no_auth = rpc_batch(
        &client,
        addr,
        None,
        serde_json::json!([{
            "GetForumThread": {
                "room": room_id,
                "thread_tag": "cli-read-test",
                "offset": null,
                "limit": null,
                "since": null,
                "before": null,
                "actor": null,
                "post_id": null
            }
        }]),
    )
    .await;
    let line_na = &no_auth["results"][0];
    assert_eq!(
        line_na["ok"], false,
        "expected failure without bearer: {:?}",
        line_na
    );
    assert_eq!(line_na["error"], "room not found");

    let with_auth = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "GetForumThread": {
                "room": room_id,
                "thread_tag": "cli-read-test",
                "offset": null,
                "limit": null,
                "since": null,
                "before": null,
                "actor": null,
                "post_id": null
            }
        }]),
    )
    .await;
    let line_ok = &with_auth["results"][0];
    assert_eq!(
        line_ok["ok"], true,
        "expected success with bearer: {:?}",
        line_ok
    );
    let total = line_ok["result"]["ForumThread"]["total"].as_u64().unwrap();
    assert!(total >= 1);
}

#[tokio::test]
async fn test_private_room_read_requires_explicit_view_capability() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();
    let bearer = test_bearer();

    // Create private room as testuser.
    let create = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "RoomCreate": { "slug": "private-read-acl" }
        }]),
    )
    .await;
    let room_id = create["results"][0]["result"]["RoomCreated"]["room_id"]
        .as_str()
        .unwrap()
        .to_string();

    // Ingest private content while owner still has full caps.
    let post = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "Post": {
                "room": room_id,
                "thread_tag": "main",
                "delegate": "00000000-0000-0000-0000-000000000000:test:local/test",
                "text": "~/secret/item {top secret}\nprivate prose\n",
                "return_rank_diff": false
            }
        }]),
    )
    .await;
    assert_eq!(post["results"][0]["ok"], true);

    // Remove explicit view cap from owner (still has Manage/Post/Vote/AddItem).
    let revoke_view = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "RoomRevoke": {
                "room": room_id,
                "username": "testuser",
                "capability": "view"
            }
        }]),
    )
    .await;
    assert_eq!(revoke_view["results"][0]["ok"], true);

    // Private reads must now be denied with not-found semantics.
    let item_read = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "GetGardenItem": {
                "room": room_id,
                "item_path": "~/secret/item",
                "full": true
            }
        }]),
    )
    .await;
    let line = &item_read["results"][0];
    assert_eq!(line["ok"], false);
    assert_eq!(line["error"], "room not found");

    // Search should not leak private posts when caller lacks explicit view.
    let search = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "Search": { "query": "secret" }
        }]),
    )
    .await;
    let posts = search["results"][0]["result"]["Search"]["posts"]
        .as_array()
        .unwrap();
    assert!(
        posts.is_empty(),
        "search should hide private posts without view capability"
    );

    // Feed should likewise hide private posts (keyed by delegate on those ingests).
    let feed = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "GetFeed": { "delegate": "00000000-0000-0000-0000-000000000000:test:local/test", "limit": 20 }
        }]),
    )
    .await;
    let feed_posts = feed["results"][0]["result"]["Feed"]["posts"]
        .as_array()
        .unwrap();
    assert!(
        feed_posts.is_empty(),
        "feed should hide private posts without view capability"
    );
}

#[tokio::test]
async fn test_feed_since_last_post_is_scoped_to_delegate() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();
    let bearer = test_bearer();

    let d1 = "00000000-0000-0000-0000-0000000000a1:feedtest:local/model-a";
    let d2 = "00000000-0000-0000-0000-0000000000a2:feedtest:local/model-b";

    let p1 = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "Post": {
                "room": "public",
                "thread_tag": "feed-delegate-test",
                "delegate": d1,
                "text": "#feed-delegate-test\nalpha marker for d1\n",
                "return_rank_diff": false
            }
        }]),
    )
    .await;
    assert_eq!(p1["results"][0]["ok"], true, "first post: {:?}", p1);

    let p2 = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "Post": {
                "room": "public",
                "thread_tag": "feed-delegate-test",
                "delegate": d2,
                "text": "#feed-delegate-test\nbeta marker for d2\n",
                "return_rank_diff": false
            }
        }]),
    )
    .await;
    assert_eq!(p2["results"][0]["ok"], true, "second post: {:?}", p2);

    // Since d1 last posted first, the feed for d1 should include everything after that (here: d2's post).
    let feed = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "GetFeed": { "delegate": d1, "limit": 20 }
        }]),
    )
    .await;
    let line = &feed["results"][0];
    assert_eq!(line["ok"], true, "GetFeed failed: {:?}", line);
    let delegate = line["result"]["Feed"]["delegate"].as_str().unwrap();
    assert_eq!(delegate, d1);
    let bodies: String = line["result"]["Feed"]["posts"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|p| p["body"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        bodies.contains("beta marker for d2"),
        "expected d2's post after d1's cutoff, got: {bodies}"
    );
    assert!(
        !bodies.contains("alpha marker for d1"),
        "d1's own prior post should not appear after cutoff, got: {bodies}"
    );

    let d_other = "00000000-0000-0000-0000-0000000000ff:feedtest:local/other";
    let steal = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "GetFeed": { "delegate": d_other, "limit": 5 }
        }]),
    )
    .await;
    let steal_line = &steal["results"][0];
    assert_eq!(
        steal_line["ok"], false,
        "expected rejection for unbound delegate: {:?}",
        steal_line
    );
    assert_eq!(steal_line["error"], "not your delegate");

    let no_auth = rpc_batch(
        &client,
        addr,
        None,
        serde_json::json!([{
            "GetFeed": { "delegate": d1, "limit": 5 }
        }]),
    )
    .await;
    let na = &no_auth["results"][0];
    assert_eq!(na["ok"], false, "GetFeed without bearer: {:?}", na);
    assert_eq!(na["error"], "missing Authorization header");
}

#[tokio::test]
async fn test_feed_without_delegate_uses_principal_last_post_including_delegate() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();
    let bearer = test_bearer();

    let d = "00000000-0000-0000-0000-0000000000b1:principalfeed:local/model";
    let p1 = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "Post": {
                "room": "public",
                "thread_tag": "principal-feed-test",
                "delegate": d,
                "text": "#principal-feed-test\nfirst delegate post\n",
                "return_rank_diff": false
            }
        }]),
    )
    .await;
    assert_eq!(p1["results"][0]["ok"], true, "{:?}", p1);

    let p2 = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "Post": {
                "room": "public",
                "thread_tag": "principal-feed-test",
                "delegate": d,
                "text": "#principal-feed-test\nsecond delegate post\n",
                "return_rank_diff": false
            }
        }]),
    )
    .await;
    assert_eq!(p2["results"][0]["ok"], true, "{:?}", p2);

    // Argless GetFeed: cutoff is last ingest by this principal (even if every post used --delegate),
    // so revisiting an old chat with only a token still gets a bounded "since", not full history.
    let feed = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "GetFeed": { "limit": 20 }
        }]),
    )
    .await;
    let line = &feed["results"][0];
    assert_eq!(line["ok"], true, "{:?}", line);
    assert!(
        line["result"]["Feed"]["delegate"].is_null(),
        "principal-wide feed omits delegate in JSON: {:?}",
        line["result"]["Feed"]
    );
    assert!(
        line["result"]["Feed"]["since"].is_number(),
        "expected since from principal's last post (including delegate ingests), got: {:?}",
        line["result"]["Feed"]["since"]
    );
    let posts = line["result"]["Feed"]["posts"].as_array().unwrap();
    assert!(
        posts.is_empty(),
        "nothing is strictly newer than the latest own post; got {} posts",
        posts.len()
    );
}

#[tokio::test]
async fn test_private_room_thread_urls_use_t_segment() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let bearer = test_bearer();

    let create = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "RoomCreate": { "slug": "url-shape" }
        }]),
    )
    .await;
    let room_id = create["results"][0]["result"]["RoomCreated"]["room_id"]
        .as_str()
        .unwrap()
        .to_string();
    let room_seg = room_route_segment(&room_id).unwrap();

    let rpc = ui_post_ingest_rpc(&room_id, "main-thread", "private post via web");
    let post = client
        .post(format!("http://{addr}/ui"))
        .header("Authorization", format!("Bearer {bearer}"))
        .form(&[("__rpc__", rpc.as_str())])
        .send()
        .await
        .unwrap();

    assert_eq!(post.status(), reqwest::StatusCode::OK);
    assert_eq!(
        post.headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/javascript; charset=utf-8")
    );
    let post_js = post.text().await.unwrap();
    let location = format!("/r/{room_seg}/t/main-thread");
    assert!(post_js.contains(&format!("window.location = {:?};", location)));
    assert!(post_js.contains("#room-thread-feed"));
    assert!(post_js.contains("#thread-feed-region"));

    let thread_page = client
        .get(format!("http://{addr}{location}"))
        .header("Authorization", format!("Bearer {bearer}"))
        .send()
        .await
        .unwrap();
    assert!(thread_page.status().is_success());
    let body = thread_page.text().await.unwrap();
    assert!(body.contains("#main-thread"));
    assert!(body.contains("private post via web"));
}

#[tokio::test]
async fn test_private_room_post_links_use_private_garden_routes() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let bearer = test_bearer();

    let create = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "RoomCreate": { "slug": "private-garden-links" }
        }]),
    )
    .await;
    let room_id = create["results"][0]["result"]["RoomCreated"]["room_id"]
        .as_str()
        .unwrap()
        .to_string();
    let room_seg = room_route_segment(&room_id).unwrap();

    let rpc = ui_post_ingest_rpc(
        &room_id,
        "garden-thread",
        "~/secret/item {classified}\n~/secret/other {other body}\n{because}\n~/secret/item 3:1 ~/secret/other\n",
    );
    let post = client
        .post(format!("http://{addr}/ui"))
        .header("Authorization", format!("Bearer {bearer}"))
        .form(&[("__rpc__", rpc.as_str())])
        .send()
        .await
        .unwrap();
    assert_eq!(post.status(), reqwest::StatusCode::OK);

    let thread_page = client
        .get(format!("http://{addr}/r/{room_seg}/t/garden-thread"))
        .header("Authorization", format!("Bearer {bearer}"))
        .send()
        .await
        .unwrap();
    assert!(thread_page.status().is_success());
    let body = thread_page.text().await.unwrap();
    assert!(body.contains(&format!("/r/{room_seg}/~/secret/item")));
    assert!(!body.contains("href=\"/~/secret/item\""));

    let garden_page = client
        .get(format!("http://{addr}/r/{room_seg}/~/secret/item"))
        .header("Authorization", format!("Bearer {bearer}"))
        .send()
        .await
        .unwrap();
    assert!(garden_page.status().is_success());
    let garden_body = garden_page.text().await.unwrap();
    assert!(garden_body.contains("classified"));
    assert!(garden_body.contains(&format!("/r/{room_seg}/t/garden-thread")));
}

#[tokio::test]
async fn test_private_room_garden_root_lists_top_level_tilde_children() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let bearer = test_bearer();

    let create = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "RoomCreate": { "slug": "garden-root-list" }
        }]),
    )
    .await;
    let room_id = create["results"][0]["result"]["RoomCreated"]["room_id"]
        .as_str()
        .unwrap()
        .to_string();
    let room_seg = room_route_segment(&room_id).unwrap();

    let rpc = ui_post_ingest_rpc(
        &room_id,
        "ing",
        "~/test1 {wow}\n~/test2 {wow2}\n{because}\n~/test1 2:1 ~/test2\n",
    );
    let post = client
        .post(format!("http://{addr}/ui"))
        .header("Authorization", format!("Bearer {bearer}"))
        .form(&[("__rpc__", rpc.as_str())])
        .send()
        .await
        .unwrap();
    assert_eq!(post.status(), reqwest::StatusCode::OK);

    let root_page = client
        .get(format!("http://{addr}/r/{room_seg}/~"))
        .header("Authorization", format!("Bearer {bearer}"))
        .send()
        .await
        .unwrap();
    assert!(root_page.status().is_success());
    let body = root_page.text().await.unwrap();
    assert!(
        body.contains("ranked child groups"),
        "expected garden child panel: {}",
        body.len()
    );
    assert!(body.contains("~/test1"));
    assert!(body.contains("~/test2"));
    assert!(body.contains("ordering 1"));
}

#[tokio::test]
async fn test_empty_private_room_garden_returns_404() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let bearer = test_bearer();

    let create = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "RoomCreate": { "slug": "empty-garden-room" }
        }]),
    )
    .await;
    let room_id = create["results"][0]["result"]["RoomCreated"]["room_id"]
        .as_str()
        .unwrap()
        .to_string();
    let room_seg = room_route_segment(&room_id).unwrap();

    let root = client
        .get(format!("http://{addr}/r/{room_seg}/~"))
        .header("Authorization", format!("Bearer {bearer}"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        root.status(),
        reqwest::StatusCode::NOT_FOUND,
        "no ingest yet → no room scope content → must not fall back to public garden"
    );
}

#[tokio::test]
async fn test_thread_graduate_private_to_public() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();
    let bearer = test_bearer();

    let create = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "RoomCreate": { "slug": "graduate-me" }
        }]),
    )
    .await;
    let room_id = create["results"][0]["result"]["RoomCreated"]["room_id"]
        .as_str()
        .unwrap()
        .to_string();

    let thread_tag = "beta-thread";
    let post_text = format!(
        "# {thread_tag}\n\n~/graduate-demo/item-a {{first item}}\n~/graduate-demo/item-b {{second item}}\n{{because test}}\n~/graduate-demo/item-a 2:1 ~/graduate-demo/item-b\n"
    );

    let post = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "Post": {
                "room": room_id,
                "thread_tag": thread_tag,
                "delegate": "00000000-0000-0000-0000-000000000000:test:local/test",
                "text": post_text,
                "return_rank_diff": false
            }
        }]),
    )
    .await;
    assert_eq!(post["results"][0]["ok"], true, "{:?}", post["results"][0]);

    let grad = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "ThreadGraduate": {
                "room": room_id,
                "thread_tag": thread_tag
            }
        }]),
    )
    .await;
    assert_eq!(grad["results"][0]["ok"], true, "{:?}", grad["results"][0]);
    let result = &grad["results"][0]["result"]["ThreadGraduatedOk"];
    assert_eq!(result["thread_tag"], thread_tag);
    assert_eq!(result["posts_copied"], 1);
    assert!(
        result["web"]
            .as_str()
            .unwrap_or("")
            .ends_with(&format!("/t/{thread_tag}")),
        "expected public thread URL, got {:?}",
        result["web"]
    );

    let public_thread = rpc_batch(
        &client,
        addr,
        None,
        serde_json::json!([{
            "GetForumThread": {
                "room": "public",
                "thread_tag": thread_tag,
                "offset": null,
                "limit": null,
                "since": null,
                "before": null,
                "actor": null,
                "post_id": null
            }
        }]),
    )
    .await;
    assert_eq!(public_thread["results"][0]["ok"], true);
    let items = public_thread["results"][0]["result"]["ForumThread"]["items"]
        .as_array()
        .unwrap();
    assert_eq!(items.len(), 1);

    let public_item = rpc_batch(
        &client,
        addr,
        None,
        serde_json::json!([{
            "GetGardenItem": {
                "room": "public",
                "item_path": "~/graduate-demo/item-a",
                "full": true
            }
        }]),
    )
    .await;
    assert_eq!(public_item["results"][0]["ok"], true, "{:?}", public_item["results"][0]);
    let body = public_item["results"][0]["result"]["GardenItem"]["body"]
        .as_str()
        .unwrap_or("");
    assert!(body.contains("first item"), "expected graduated item body, got {body:?}");

    let post_again = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "Post": {
                "room": room_id,
                "thread_tag": thread_tag,
                "delegate": "00000000-0000-0000-0000-000000000000:test:local/test",
                "text": "should be blocked\n",
                "return_rank_diff": false
            }
        }]),
    )
    .await;
    assert_eq!(post_again["results"][0]["ok"], false);
    assert_eq!(
        post_again["results"][0]["error"],
        "thread graduated to public"
    );

    let grad_again = rpc_batch(
        &client,
        addr,
        Some(&bearer),
        serde_json::json!([{
            "ThreadGraduate": {
                "room": room_id,
                "thread_tag": thread_tag
            }
        }]),
    )
    .await;
    assert_eq!(grad_again["results"][0]["ok"], false);
    assert_eq!(grad_again["results"][0]["error"], "thread already graduated");
}

