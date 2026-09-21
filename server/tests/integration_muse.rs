mod support;

use support::*;

async fn muse_get(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    path: &str,
    bearer: Option<&str>,
) -> reqwest::Response {
    let mut req = client.get(format!("http://{addr}{path}"));
    if let Some(b) = bearer {
        req = req.header("Authorization", format!("Bearer {b}"));
    }
    req.send().await.unwrap()
}

async fn muse_post(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    tool: &str,
    body: serde_json::Value,
    bearer: Option<&str>,
) -> reqwest::Response {
    muse_post_auth(client, addr, tool, body, bearer, None).await
}

async fn muse_post_auth(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    tool: &str,
    body: serde_json::Value,
    bearer: Option<&str>,
    session: Option<&str>,
) -> reqwest::Response {
    let mut req = client
        .post(format!("http://{addr}/muse/v1/{tool}"))
        .json(&body);
    if let Some(b) = bearer {
        req = req.header("Authorization", format!("Bearer {b}"));
    }
    if let Some(s) = session {
        req = req.header("X-Slug-Session", s);
    }
    req.send().await.unwrap()
}

fn assert_no_user_token_copy(text: &str) {
    let lower = text.to_lowercase();
    assert!(
        !text.contains("slug_"),
        "user-facing Muse copy must not mention slug_ tokens: {text}"
    );
    assert!(
        !lower.contains("secure credential"),
        "must not tell Muse to paste into a credential vault: {text}"
    );
    for phrase in [
        "paste a token",
        "paste the token",
        "paste it into",
        "bearer <token>",
        "bearer token",
        "paste this into muse",
    ] {
        assert!(
            !lower.contains(phrase),
            "must not tell anyone to paste a token ({phrase}): {text}"
        );
    }
}

#[tokio::test]
async fn muse_status_and_public_spec_need_no_auth() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();

    let index = muse_get(&client, addr, "/muse/v1", None)
        .await
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(index["name"], "slug-social");
    assert_eq!(index["kind"], "muse-connector");
    assert!(index["status"]
        .as_str()
        .unwrap()
        .ends_with("/muse/v1/status"));
    assert!(index["openapi"]
        .as_str()
        .unwrap()
        .ends_with("/muse/v1/openapi.json"));
    assert!(index["mcp"].as_str().unwrap().ends_with("/mcp"));

    let status = muse_get(&client, addr, "/muse/v1/status", None).await;
    assert!(status.status().is_success(), "{}", status.status());
    let status_json: serde_json::Value = status.json().await.unwrap();
    assert_eq!(status_json["ok"], true);
    assert_eq!(status_json["status"], "ok");

    let spec = muse_get(&client, addr, "/muse/v1/openapi.json", None)
        .await
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(spec["openapi"], "3.0.3");
    let paths = spec["paths"].as_object().unwrap();
    for expected in [
        "/status",
        "/whoami",
        "/health",
        "/search",
        "/post_sorter",
        "/identity_start",
        "/list_rooms",
        "/get_matchup",
    ] {
        assert!(paths.contains_key(expected), "missing {expected}");
    }
    assert_eq!(paths["/post_sorter"]["post"]["x-mcp-name"], "post_sorter");
    assert_eq!(paths["/post_sorter"]["post"]["x-openWorldHint"], true);
    assert_eq!(
        spec["components"]["securitySchemes"]["oauth2"]["type"],
        "oauth2"
    );
    assert!(spec["components"]["securitySchemes"]["bearerAuth"].is_null());
    assert_no_user_token_copy(spec["info"]["description"].as_str().unwrap());

    let docs = muse_get(&client, addr, "/muse/v1/docs.md", None).await;
    assert!(docs.status().is_success());
    assert!(docs
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("text/markdown"));
    let md = docs.text().await.unwrap();
    assert!(md.contains("openapi.json"));
    assert!(md.contains("/status"));
    assert!(md.contains("login_url"), "{md}");
    assert!(md.contains("identity_start"), "{md}");
    assert_no_user_token_copy(&md);

    let listing = muse_get(&client, addr, "/muse", None).await;
    assert!(listing.status().is_success(), "{}", listing.status());
    let html = listing.text().await.unwrap();
    assert!(html.contains("/muse/v1/openapi.json"), "{html}");
    assert!(html.contains("muse.ai/platform"), "{html}");
    assert!(html.contains("login_url"), "{html}");
    assert!(html.contains("identity_start"), "{html}");
    assert!(!html.contains("Paste this into Muse"), "{html}");
    assert!(!html.contains("slug_ bearer"), "{html}");
    assert!(!html.to_lowercase().contains("secure credential"), "{html}");
    assert!(!html.to_lowercase().contains("bearer token"), "{html}");

    let md_alias = muse_get(&client, addr, "/muse.md", None).await;
    assert!(md_alias.status().is_success());
    let alias = md_alias.text().await.unwrap();
    assert!(alias.contains("openapi.json"));
    assert_no_user_token_copy(&alias);

    assert!(index["instructions"]
        .as_str()
        .unwrap()
        .contains("identity_start"));
    assert_no_user_token_copy(index["instructions"].as_str().unwrap());
    assert_no_user_token_copy(index["auth"].as_str().unwrap());
}

#[tokio::test]
async fn muse_whoami_is_the_credential_check() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();

    let unauth = muse_get(&client, addr, "/muse/v1/whoami", None).await;
    assert_eq!(unauth.status(), reqwest::StatusCode::UNAUTHORIZED);
    let challenge = unauth
        .headers()
        .get("www-authenticate")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(challenge.contains("resource_metadata="), "{challenge}");
    let body: serde_json::Value = unauth.json().await.unwrap();
    assert!(body["error"].as_str().unwrap().contains("Link your slug"));
    assert!(
        body["next"].as_str().unwrap().contains("identity_start"),
        "{body}"
    );
    assert!(
        body["next"].as_str().unwrap().contains("login_url"),
        "{body}"
    );
    assert!(
        body["next"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("never ask"),
        "{body}"
    );

    let ok = muse_get(&client, addr, "/muse/v1/whoami", Some(&test_bearer()))
        .await
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(ok["user"], "testuser");
    assert!(ok["delegates"].is_array());
}

const TEST_DELEGATE: &str = "00000000-0000-0000-0000-000000000000:test:local/test";

#[tokio::test]
async fn muse_read_and_write_tools_round_trip() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();
    let bearer = test_bearer();
    let doc = "~/muse-a {alpha}\n~/muse-b {beta}\n{because tests}\n~/muse-a 2:1 ~/muse-b\n";

    let unauth = muse_post(
        &client,
        addr,
        "post_sorter",
        serde_json::json!({"thread_tag": "muse-demo", "text": doc}),
        None,
    )
    .await;
    assert_eq!(unauth.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert!(unauth.headers().get("www-authenticate").is_some());

    let missing_delegate = muse_post(
        &client,
        addr,
        "post_sorter",
        serde_json::json!({"thread_tag": "muse-demo", "text": doc}),
        Some(&bearer),
    )
    .await;
    assert_eq!(missing_delegate.status(), reqwest::StatusCode::BAD_REQUEST);
    let missing_json: serde_json::Value = missing_delegate.json().await.unwrap();
    assert!(
        missing_json["error"]
            .as_str()
            .unwrap()
            .contains("delegate is required"),
        "{missing_json}"
    );

    let posted = muse_post(
        &client,
        addr,
        "post_sorter",
        serde_json::json!({
            "thread_tag": "muse-demo",
            "text": doc,
            "delegate": TEST_DELEGATE
        }),
        Some(&bearer),
    )
    .await;
    assert!(posted.status().is_success(), "{}", posted.status());

    let search = muse_post(
        &client,
        addr,
        "search",
        serde_json::json!({"query": "muse-a"}),
        None,
    )
    .await;
    assert!(search.status().is_success(), "{}", search.status());
    let found: serde_json::Value = search.json().await.unwrap();
    assert!(
        found["results"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"].as_str().unwrap().contains("muse-a")),
        "{found}"
    );

    let rank = muse_post(
        &client,
        addr,
        "get_rank",
        serde_json::json!({"parent_path": "~"}),
        None,
    )
    .await;
    assert!(rank.status().is_success());

    let item = muse_post(
        &client,
        addr,
        "get_item",
        serde_json::json!({"item_path": "muse-a"}),
        None,
    )
    .await;
    assert!(item.status().is_success(), "{}", item.status());

    let minted = muse_post(
        &client,
        addr,
        "identity_start",
        serde_json::json!({"rig": "muse", "model": "meta/muse"}),
        Some(&bearer),
    )
    .await;
    assert!(minted.status().is_success(), "{}", minted.status());
    let minted_json: serde_json::Value = minted.json().await.unwrap();
    assert_eq!(minted_json["phase"], "ready");
    assert!(minted_json["delegate"]
        .as_str()
        .unwrap()
        .contains(":muse:meta/muse"));

    let unknown = muse_post(&client, addr, "not_a_tool", serde_json::json!({}), None).await;
    assert_eq!(unknown.status(), reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn muse_oauth_accepts_muse_redirect() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let resp = client
        .get(format!("http://{addr}/oauth/authorize"))
        .query(&[
            ("response_type", "code"),
            ("client_id", "https://muse.ai/oauth/client.json"),
            ("redirect_uri", "https://muse.ai/connectors/oauth/callback"),
            (
                "code_challenge",
                "LGZJYPXoCfqeQ2pG8EKrCEHgLugRSKQ1j3qQQB8GYeU",
            ),
            ("code_challenge_method", "S256"),
            ("state", "muse-state"),
            ("scope", "slug.read slug.write"),
            ("resource", "http://127.0.0.1:8080/mcp"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::TEMPORARY_REDIRECT);
    let loc = resp
        .headers()
        .get("location")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(loc.contains("/auth/login?session="), "{loc}");

    let muse_resource = client
        .get(format!("http://{addr}/oauth/authorize"))
        .query(&[
            ("response_type", "code"),
            ("client_id", "https://muse.ai/oauth/client.json"),
            (
                "redirect_uri",
                "https://agent.meta.ai/connectors/oauth/callback",
            ),
            (
                "code_challenge",
                "LGZJYPXoCfqeQ2pG8EKrCEHgLugRSKQ1j3qQQB8GYeU",
            ),
            ("code_challenge_method", "S256"),
            ("state", "muse-v1"),
            ("scope", "slug.read slug.write"),
            ("resource", "http://127.0.0.1:8080/muse/v1"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(
        muse_resource.status(),
        reqwest::StatusCode::TEMPORARY_REDIRECT
    );
    assert!(muse_resource
        .headers()
        .get("location")
        .unwrap()
        .to_str()
        .unwrap()
        .contains("/auth/login?session="));

    let well_known = client
        .get(format!(
            "http://{addr}/.well-known/oauth-protected-resource/muse/v1"
        ))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert!(well_known["resource"]
        .as_str()
        .unwrap()
        .ends_with("/muse/v1"));
}

#[tokio::test]
async fn muse_identity_start_shows_login_url_never_a_token() {
    let (addr, _tmp, _log, state, _handle) = create_test_server_with_state().await;
    let client = reqwest::Client::new();

    let started = muse_post(
        &client,
        addr,
        "identity_start",
        serde_json::json!({"rig": "muse", "model": "meta/muse"}),
        None,
    )
    .await;
    assert!(started.status().is_success(), "{}", started.status());
    let started_json: serde_json::Value = started.json().await.unwrap();
    assert_eq!(started_json["phase"], "present_oauth_url_to_user");
    let login_url = started_json["login_url"].as_str().unwrap();
    assert!(login_url.contains("/auth/login?session="), "{started_json}");
    assert!(started_json["poll_url"].is_null(), "{started_json}");
    assert!(started_json["token"].is_null(), "{started_json}");
    let instruction = started_json["instruction"].as_str().unwrap().to_lowercase();
    assert!(instruction.contains("login_url"), "{started_json}");
    assert!(
        instruction.contains("never") && instruction.contains("token"),
        "{started_json}"
    );
    let session = started_json["session"].as_str().unwrap().to_string();

    let pending = muse_post(
        &client,
        addr,
        "identity_poll",
        serde_json::json!({"session": session}),
        None,
    )
    .await;
    assert!(pending.status().is_success());
    let pending_json: serde_json::Value = pending.json().await.unwrap();
    assert_eq!(pending_json["complete"], false);
    assert!(pending_json["login_url"]
        .as_str()
        .unwrap()
        .contains("/auth/login?session="));
    assert!(pending_json["token"].is_null());

    let unready = muse_get(&client, addr, "/muse/v1/whoami", None).await;
    assert_eq!(unready.status(), reqwest::StatusCode::UNAUTHORIZED);

    {
        let mut sessions = state.pending_sessions.write().await;
        let pending = sessions.get_mut(&session).expect("session");
        pending.complete = Some(("testuser".into(), test_bearer()));
    }

    let done = muse_post(
        &client,
        addr,
        "identity_poll",
        serde_json::json!({"session": session}),
        None,
    )
    .await;
    let done_json: serde_json::Value = done.json().await.unwrap();
    assert_eq!(done_json["phase"], "complete");
    assert_eq!(done_json["user"], "testuser");
    assert!(done_json["token"].is_null(), "{done_json}");
    assert!(done_json["delegate"]
        .as_str()
        .unwrap()
        .contains(":muse:meta/muse"));

    let linked = client
        .get(format!("http://{addr}/muse/v1/whoami"))
        .header("X-Slug-Session", &session)
        .send()
        .await
        .unwrap();
    assert!(linked.status().is_success(), "{}", linked.status());
    let linked_json: serde_json::Value = linked.json().await.unwrap();
    assert_eq!(linked_json["user"], "testuser");

    let posted = muse_post_auth(
        &client,
        addr,
        "post_sorter",
        serde_json::json!({
            "thread_tag": "muse-oauth",
            "text": "~/muse-link-a {alpha}\n~/muse-link-b {beta}\n{because session}\n~/muse-link-a 2:1 ~/muse-link-b\n",
            "delegate": done_json["delegate"]
        }),
        None,
        Some(&session),
    )
    .await;
    assert!(posted.status().is_success(), "{}", posted.status());
}
