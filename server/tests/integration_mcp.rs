mod support;

use slugsocial_server::mcp::oauth::{finish_mcp_oauth_if_pending, pkce_s256_challenge};
use support::*;

async fn mcp_call(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    method: &str,
    params: serde_json::Value,
    bearer: Option<&str>,
) -> serde_json::Value {
    let url = format!("http://{addr}/mcp");
    let mut req = client.post(url).json(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params
    }));
    if let Some(b) = bearer {
        req = req.header("Authorization", format!("Bearer {b}"));
    }
    let response = req.send().await.unwrap();
    assert!(
        response.status().is_success(),
        "mcp http {} {}",
        response.status(),
        response.text().await.unwrap_or_default()
    );
    response.json().await.unwrap()
}

async fn tool_call(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    name: &str,
    arguments: serde_json::Value,
    bearer: Option<&str>,
) -> serde_json::Value {
    let body = mcp_call(
        client,
        addr,
        "tools/call",
        serde_json::json!({"name": name, "arguments": arguments}),
        bearer,
    )
    .await;
    body["result"].clone()
}

#[tokio::test]
async fn mcp_initialize_and_lists_v1_tools() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();

    let init = mcp_call(
        &client,
        addr,
        "initialize",
        serde_json::json!({
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "clientInfo": {"name": "test", "version": "0"}
        }),
        None,
    )
    .await;
    assert_eq!(init["result"]["serverInfo"]["name"], "slug-social");
    assert!(init["result"]["instructions"]
        .as_str()
        .unwrap()
        .contains("list_rooms"));
    assert!(init["result"]["instructions"]
        .as_str()
        .unwrap()
        .contains("read_room"));
    assert!(init["result"]["instructions"]
        .as_str()
        .unwrap()
        .contains("identity_start"));
    assert!(init["result"]["instructions"]
        .as_str()
        .unwrap()
        .contains("get_matchup"));
    assert!(init["result"]["instructions"]
        .as_str()
        .unwrap()
        .contains("ask the human"));

    let listed = mcp_call(&client, addr, "tools/list", serde_json::json!({}), None).await;
    let tools = listed["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    for expected in [
        "whoami",
        "identity_start",
        "identity_poll",
        "list_rooms",
        "read_room",
        "get_feed",
        "get_matchup",
        "search",
        "fetch",
        "list_threads",
        "get_thread",
        "get_rank",
        "get_item",
        "get_pair",
        "check_sorter",
        "create_room",
        "grant_room",
        "audit_room",
        "post_sorter",
        "redact_post",
    ] {
        assert!(names.contains(&expected), "missing {expected} in {names:?}");
    }
    let post = tools.iter().find(|t| t["name"] == "post_sorter").unwrap();
    assert_eq!(post["annotations"]["readOnlyHint"], false);
    assert_eq!(post["annotations"]["openWorldHint"], true);
    assert_eq!(post["annotations"]["destructiveHint"], false);
    assert_eq!(post["securitySchemes"][0]["type"], "oauth2");
    let required = post["inputSchema"]["required"].as_array().unwrap();
    assert!(required.iter().any(|v| v == "delegate"), "{required:?}");
    let create = tools.iter().find(|t| t["name"] == "create_room").unwrap();
    assert_eq!(create["annotations"]["openWorldHint"], false);
    let search = tools.iter().find(|t| t["name"] == "search").unwrap();
    assert_eq!(search["annotations"]["readOnlyHint"], true);
    assert_eq!(search["securitySchemes"][0]["type"], "oauth2");
    assert_eq!(search["securitySchemes"][0]["scopes"][0], "slug.read");
    assert_eq!(search["securitySchemes"][1]["type"], "noauth");
    let list_rooms = tools.iter().find(|t| t["name"] == "list_rooms").unwrap();
    assert_eq!(list_rooms["securitySchemes"][0]["type"], "oauth2");
    assert_eq!(list_rooms["securitySchemes"][0]["scopes"][0], "slug.read");
    let read_room = tools.iter().find(|t| t["name"] == "read_room").unwrap();
    assert_eq!(read_room["annotations"]["readOnlyHint"], true);
    assert_eq!(read_room["securitySchemes"][0]["scopes"][0], "slug.read");
    assert!(read_room["inputSchema"]["required"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v == "room_id"));
}

const TEST_DELEGATE: &str = "00000000-0000-0000-0000-000000000000:test:local/test";
const OTHER_DELEGATE: &str = "11111111-1111-1111-1111-111111111111:test:local/other";

#[tokio::test]
async fn mcp_read_and_write_tools_round_trip() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::new();
    let bearer = test_bearer();
    let doc = "~/mcp-a {alpha}\n~/mcp-b {beta}\n{because tests}\n~/mcp-a 2:1 ~/mcp-b\n";

    let unauth = tool_call(
        &client,
        addr,
        "post_sorter",
        serde_json::json!({"thread_tag": "mcp-demo", "text": doc}),
        None,
    )
    .await;
    assert_eq!(unauth["isError"], true);
    let challenge = unauth["_meta"]["mcp/www_authenticate"][0].as_str().unwrap();
    assert!(challenge.contains("resource_metadata="), "{challenge}");
    assert!(challenge.contains("error="), "{challenge}");

    let missing_delegate = tool_call(
        &client,
        addr,
        "post_sorter",
        serde_json::json!({"thread_tag": "mcp-demo", "text": doc}),
        Some(&bearer),
    )
    .await;
    assert_eq!(missing_delegate["isError"], true, "{missing_delegate}");
    assert!(
        missing_delegate["structuredContent"]["error"]
            .as_str()
            .unwrap()
            .contains("delegate is required"),
        "{missing_delegate}"
    );

    let bad_delegate = tool_call(
        &client,
        addr,
        "post_sorter",
        serde_json::json!({
            "thread_tag": "mcp-demo",
            "text": doc,
            "delegate": "not-a-delegate"
        }),
        Some(&bearer),
    )
    .await;
    assert_eq!(bad_delegate["isError"], true, "{bad_delegate}");
    assert!(
        bad_delegate["structuredContent"]["error"]
            .as_str()
            .unwrap()
            .contains("invalid delegate"),
        "{bad_delegate}"
    );

    let posted = tool_call(
        &client,
        addr,
        "post_sorter",
        serde_json::json!({
            "thread_tag": "mcp-demo",
            "text": doc,
            "delegate": TEST_DELEGATE
        }),
        Some(&bearer),
    )
    .await;
    assert_eq!(posted["isError"], false, "{posted}");
    let post_id = posted["structuredContent"]["PostOk"]["post_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(posted["structuredContent"]["actor"], "testuser");
    assert_eq!(posted["structuredContent"]["delegate"], TEST_DELEGATE);

    let found = tool_call(
        &client,
        addr,
        "search",
        serde_json::json!({"query": "mcp-a"}),
        None,
    )
    .await;
    assert_eq!(found["isError"], false, "{found}");
    let results = found["structuredContent"]["results"].as_array().unwrap();
    let post_hit = results
        .iter()
        .find(|r| r["id"].as_str() == Some(&format!("post:{post_id}")))
        .unwrap_or_else(|| panic!("missing post hit: {results:?}"));
    assert_eq!(post_hit["actor"], "testuser");
    assert_eq!(post_hit["delegate"], TEST_DELEGATE);
    assert!(
        results
            .iter()
            .any(|r| r["id"].as_str().unwrap().contains("mcp-a")),
        "{results:?}"
    );

    let item = tool_call(
        &client,
        addr,
        "fetch",
        serde_json::json!({"id": "item:~/mcp-a"}),
        None,
    )
    .await;
    assert_eq!(item["isError"], false, "{item}");
    assert!(item["structuredContent"]["text"]
        .as_str()
        .unwrap()
        .contains("alpha"));
    assert!(item["structuredContent"]["url"]
        .as_str()
        .unwrap()
        .contains("mcp-a"));

    let post = tool_call(
        &client,
        addr,
        "fetch",
        serde_json::json!({"id": format!("post:{post_id}")}),
        None,
    )
    .await;
    assert_eq!(post["isError"], false, "{post}");
    assert!(post["structuredContent"]["text"]
        .as_str()
        .unwrap()
        .contains("mcp-a"));
    assert_eq!(post["structuredContent"]["actor"], "testuser");
    assert_eq!(post["structuredContent"]["delegate"], TEST_DELEGATE);
    assert_eq!(post["structuredContent"]["metadata"]["actor"], "testuser");
    assert_eq!(
        post["structuredContent"]["metadata"]["delegate"],
        TEST_DELEGATE
    );

    let rank = tool_call(
        &client,
        addr,
        "get_rank",
        serde_json::json!({"parent_path": "~"}),
        None,
    )
    .await;
    assert_eq!(rank["isError"], false, "{rank}");

    let pair = tool_call(
        &client,
        addr,
        "get_pair",
        serde_json::json!({"parent_path": "~"}),
        None,
    )
    .await;
    assert_eq!(pair["isError"], false, "{pair}");

    let matchup = tool_call(
        &client,
        addr,
        "get_matchup",
        serde_json::json!({"item_path": "~/mcp-a"}),
        None,
    )
    .await;
    assert_eq!(matchup["isError"], false, "{matchup}");
    let votes = matchup["structuredContent"]["Matchup"]["votes"]
        .as_array()
        .unwrap();
    assert!(!votes.is_empty(), "{matchup}");
    assert!(
        votes
            .iter()
            .any(|v| v["thread"].as_str() == Some("mcp-demo")),
        "{votes:?}"
    );

    let minted = tool_call(
        &client,
        addr,
        "identity_start",
        serde_json::json!({"rig": "cursor", "model": "anthropic/claude-sonnet-4.5"}),
        Some(&bearer),
    )
    .await;
    assert_eq!(minted["isError"], false, "{minted}");
    assert_eq!(minted["structuredContent"]["phase"], "ready");
    assert_eq!(minted["structuredContent"]["user"], "testuser");
    let minted_delegate = minted["structuredContent"]["delegate"]
        .as_str()
        .unwrap();
    assert!(
        minted_delegate.contains(":cursor:anthropic/claude-sonnet-4.5"),
        "{minted_delegate}"
    );

    let unlinked = tool_call(
        &client,
        addr,
        "identity_start",
        serde_json::json!({"rig": "claude", "model": "anthropic/claude-opus-4.6"}),
        None,
    )
    .await;
    assert_eq!(unlinked["isError"], false, "{unlinked}");
    assert_eq!(
        unlinked["structuredContent"]["phase"],
        "present_oauth_url_to_user"
    );
    assert!(unlinked["structuredContent"]["login_url"]
        .as_str()
        .unwrap()
        .contains("/auth/login?session="));
    let session = unlinked["structuredContent"]["session"]
        .as_str()
        .unwrap()
        .to_string();
    let pending = tool_call(
        &client,
        addr,
        "identity_poll",
        serde_json::json!({"session": session}),
        None,
    )
    .await;
    assert_eq!(pending["isError"], false, "{pending}");
    assert_eq!(pending["structuredContent"]["complete"], false);

    let threads = tool_call(&client, addr, "list_threads", serde_json::json!({}), None).await;
    assert_eq!(threads["isError"], false, "{threads}");

    let thread = tool_call(
        &client,
        addr,
        "get_thread",
        serde_json::json!({"thread_tag": "mcp-demo"}),
        None,
    )
    .await;
    assert_eq!(thread["isError"], false, "{thread}");

    let check = tool_call(
        &client,
        addr,
        "check_sorter",
        serde_json::json!({"text": "{equal}\n~/mcp-a 1:1 ~/mcp-b\n"}),
        None,
    )
    .await;
    assert_eq!(check["isError"], false, "{check}");

    let redacted = tool_call(
        &client,
        addr,
        "redact_post",
        serde_json::json!({"post_id": post_id}),
        Some(&bearer),
    )
    .await;
    assert_eq!(redacted["isError"], false, "{redacted}");
}

#[tokio::test]
async fn mcp_oauth_metadata_authorize_and_pkce_token() {
    let (addr, _tmp, _log, state, _handle) = create_test_server_with_state().await;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();

    let pr = client
        .get(format!(
            "http://{addr}/.well-known/oauth-protected-resource"
        ))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert!(pr["resource"].as_str().unwrap().ends_with("/mcp"));
    assert_eq!(pr["authorization_servers"][0], "http://127.0.0.1:8080");

    let as_meta = client
        .get(format!(
            "http://{addr}/.well-known/oauth-authorization-server"
        ))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(as_meta["code_challenge_methods_supported"][0], "S256");
    assert_eq!(as_meta["token_endpoint_auth_methods_supported"][0], "none");
    assert_eq!(as_meta["client_id_metadata_document_supported"], true);

    let challenge = client
        .get(format!("http://{addr}/.well-known/openai-apps-challenge"))
        .send()
        .await
        .unwrap();
    assert_eq!(challenge.status(), reqwest::StatusCode::NOT_FOUND);

    let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    let code_challenge = pkce_s256_challenge(verifier);
    let authorize = client
        .get(format!("http://{addr}/oauth/authorize"))
        .query(&[
            ("response_type", "code"),
            ("client_id", "https://chatgpt.com/oauth/client.json"),
            ("redirect_uri", "http://127.0.0.1:9/cb"),
            ("code_challenge", code_challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("state", "xyz"),
            ("resource", "http://127.0.0.1:8080/mcp"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(authorize.status(), reqwest::StatusCode::TEMPORARY_REDIRECT);
    let loc = authorize
        .headers()
        .get("location")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(loc.contains("/auth/login?session="), "{loc}");
    let session = loc
        .split("session=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap()
        .to_string();
    let session = urlencoding::decode(&session).unwrap().into_owned();

    {
        let sessions = state.pending_sessions.read().await;
        let pending = sessions.get(&session).unwrap();
        assert!(pending.mcp_oauth.is_some());
    }

    let bearer = test_bearer();
    let redirect = finish_mcp_oauth_if_pending(&state, &session, "testuser", &bearer)
        .await
        .expect("mcp oauth finish");
    assert!(redirect.starts_with("http://127.0.0.1:9/cb?"), "{redirect}");
    assert!(redirect.contains("state=xyz"), "{redirect}");
    assert!(redirect.contains("iss="), "{redirect}");
    let code = url::Url::parse(&redirect)
        .unwrap()
        .query_pairs()
        .find(|(k, _)| k == "code")
        .unwrap()
        .1
        .into_owned();

    let token = client
        .post(format!("http://{addr}/oauth/token"))
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("code_verifier", verifier),
            ("redirect_uri", "http://127.0.0.1:9/cb"),
            ("client_id", "https://chatgpt.com/oauth/client.json"),
            ("resource", "http://127.0.0.1:8080/mcp"),
        ])
        .send()
        .await
        .unwrap();
    assert!(token.status().is_success(), "{}", token.status());
    let token_json: serde_json::Value = token.json().await.unwrap();
    assert_eq!(token_json["access_token"], bearer);
    assert_eq!(token_json["token_type"], "Bearer");

    let replay = client
        .post(format!("http://{addr}/oauth/token"))
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("code_verifier", verifier),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(replay.status(), reqwest::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn mcp_oauth_rejects_foreign_redirect() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let resp = client
        .get(format!("http://{addr}/oauth/authorize"))
        .query(&[
            ("response_type", "code"),
            ("client_id", "https://chatgpt.com/oauth/client.json"),
            ("redirect_uri", "https://evil.example/cb"),
            ("code_challenge", "abc"),
            ("code_challenge_method", "S256"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn mcp_oauth_accepts_claude_redirect() {
    let (addr, _tmp, _log, _handle) = create_test_server().await;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let resp = client
        .get(format!("http://{addr}/oauth/authorize"))
        .query(&[
            ("response_type", "code"),
            (
                "client_id",
                "https://claude.ai/oauth/mcp-oauth-client-metadata",
            ),
            ("redirect_uri", "https://claude.ai/api/mcp/auth_callback"),
            ("code_challenge", "LGZJYPXoCfqeQ2pG8EKrCEHgLugRSKQ1j3qQQB8GYeU"),
            ("code_challenge_method", "S256"),
            ("state", "B9ix7zIQCbjJTQCfADcaXjg0VrzzLftlF61gz0nbDm0"),
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
}

#[tokio::test]
async fn mcp_whoami_and_private_room_round_trip() {
    let (addr, _tmp, _log, state, _handle) = create_test_server_with_state().await;
    let client = reqwest::Client::new();
    let bearer = test_bearer();
    let other = seed_test_identity(&state, "otheruser", "othertok", "othersecret").await;

    let unauth = tool_call(&client, addr, "whoami", serde_json::json!({}), None).await;
    assert_eq!(unauth["isError"], true);
    assert!(unauth["_meta"]["mcp/www_authenticate"][0].is_string());

    let me = tool_call(
        &client,
        addr,
        "whoami",
        serde_json::json!({}),
        Some(&bearer),
    )
    .await;
    assert_eq!(me["isError"], false, "{me}");
    assert_eq!(me["structuredContent"]["user"], "testuser");
    assert_eq!(
        me["structuredContent"]["delegates"]
            .as_array()
            .unwrap()
            .len(),
        0
    );

    let public_only = tool_call(
        &client,
        addr,
        "create_room",
        serde_json::json!({"name": "nope", "visibility": "public"}),
        Some(&bearer),
    )
    .await;
    assert_eq!(public_only["isError"], true, "{public_only}");
    assert!(
        public_only["structuredContent"]["error"]
            .as_str()
            .unwrap()
            .contains("only private rooms"),
        "{public_only}"
    );

    let created = tool_call(
        &client,
        addr,
        "create_room",
        serde_json::json!({
            "name": "Secret Project",
            "visibility": "private",
            "members": ["otheruser"]
        }),
        Some(&bearer),
    )
    .await;
    assert_eq!(created["isError"], false, "{created}");
    let room_id = created["structuredContent"]["room_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(room_id.contains("/secret-project"), "{room_id}");
    assert_eq!(
        created["structuredContent"]["members_granted"][0],
        "otheruser"
    );

    let rooms = tool_call(
        &client,
        addr,
        "list_rooms",
        serde_json::json!({}),
        Some(&bearer),
    )
    .await;
    assert_eq!(rooms["isError"], false, "{rooms}");
    let listed = rooms["structuredContent"]["RoomList"]["rooms"]
        .as_array()
        .unwrap();
    assert!(
        listed.iter().any(|r| r.as_str() == Some(room_id.as_str())),
        "{listed:?}"
    );

    let audit = tool_call(
        &client,
        addr,
        "audit_room",
        serde_json::json!({"room_id": room_id}),
        Some(&bearer),
    )
    .await;
    assert_eq!(audit["isError"], false, "{audit}");
    let grants = audit["structuredContent"]["RoomAudit"]["grants"]
        .as_array()
        .unwrap();
    assert!(
        grants.iter().any(|g| g["username"] == "otheruser"
            && g["capabilities"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c == "post")),
        "{grants:?}"
    );

    let doc = "~/room-a {alpha}\n~/room-b {beta}\n{because private}\n~/room-a 2:1 ~/room-b\n";
    let posted = tool_call(
        &client,
        addr,
        "post_sorter",
        serde_json::json!({
            "room_id": room_id,
            "thread_tag": "private-demo",
            "text": doc,
            "delegate": TEST_DELEGATE
        }),
        Some(&bearer),
    )
    .await;
    assert_eq!(posted["isError"], false, "{posted}");
    let post_id = posted["structuredContent"]["PostOk"]["post_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(posted["structuredContent"]["actor"], "testuser");
    assert_eq!(posted["structuredContent"]["delegate"], TEST_DELEGATE);

    let after_bind = tool_call(
        &client,
        addr,
        "whoami",
        serde_json::json!({}),
        Some(&bearer),
    )
    .await;
    assert_eq!(
        after_bind["structuredContent"]["delegates"][0],
        TEST_DELEGATE
    );

    let stolen = tool_call(
        &client,
        addr,
        "post_sorter",
        serde_json::json!({
            "thread_tag": "steal",
            "text": "attempted steal\n",
            "delegate": TEST_DELEGATE
        }),
        Some(&other),
    )
    .await;
    assert_eq!(stolen["isError"], true, "{stolen}");
    assert!(
        stolen["structuredContent"]["error"]
            .as_str()
            .unwrap()
            .contains("delegate already bound"),
        "{stolen}"
    );

    let other_ok = tool_call(
        &client,
        addr,
        "post_sorter",
        serde_json::json!({
            "room_id": room_id,
            "thread_tag": "private-demo",
            "text": "hello from other\n",
            "delegate": OTHER_DELEGATE
        }),
        Some(&other),
    )
    .await;
    assert_eq!(other_ok["isError"], false, "{other_ok}");

    let hidden = tool_call(
        &client,
        addr,
        "get_thread",
        serde_json::json!({"thread_tag": "private-demo", "room_id": room_id}),
        None,
    )
    .await;
    assert_eq!(hidden["isError"], true, "{hidden}");
    assert_eq!(hidden["structuredContent"]["error"], "room not found");

    let leaked_search = tool_call(
        &client,
        addr,
        "search",
        serde_json::json!({"query": "because private"}),
        None,
    )
    .await;
    assert_eq!(leaked_search["isError"], false, "{leaked_search}");
    let leaked = leaked_search["structuredContent"]["results"]
        .as_array()
        .unwrap();
    assert!(
        !leaked.iter().any(|r| {
            r["id"].as_str() == Some(&format!("post:{post_id}"))
                || r["room"].as_str() == Some(room_id.as_str())
        }),
        "unauthenticated search must not return private-room hits: {leaked:?}"
    );

    let scoped_unauth = tool_call(
        &client,
        addr,
        "search",
        serde_json::json!({"query": "because private", "room_id": room_id}),
        None,
    )
    .await;
    assert_eq!(scoped_unauth["isError"], true, "{scoped_unauth}");
    assert_eq!(
        scoped_unauth["structuredContent"]["error"],
        "room not found"
    );

    let leaked_fetch = tool_call(
        &client,
        addr,
        "fetch",
        serde_json::json!({"id": format!("post:{post_id}")}),
        None,
    )
    .await;
    assert_eq!(leaked_fetch["isError"], true, "{leaked_fetch}");
    assert_eq!(
        leaked_fetch["structuredContent"]["error"],
        "room not found"
    );

    let thread = tool_call(
        &client,
        addr,
        "get_thread",
        serde_json::json!({"thread_tag": "private-demo", "room_id": room_id}),
        Some(&bearer),
    )
    .await;
    assert_eq!(thread["isError"], false, "{thread}");
    assert_eq!(thread["structuredContent"]["actor"], "testuser");
    assert_eq!(thread["structuredContent"]["delegate"], TEST_DELEGATE);

    let found = tool_call(
        &client,
        addr,
        "search",
        serde_json::json!({"query": "because private", "room_id": room_id}),
        Some(&bearer),
    )
    .await;
    assert_eq!(found["isError"], false, "{found}");
    let results = found["structuredContent"]["results"].as_array().unwrap();
    let hit = results
        .iter()
        .find(|r| r["id"].as_str() == Some(&format!("post:{post_id}")))
        .unwrap_or_else(|| panic!("missing private post: {results:?}"));
    assert_eq!(hit["actor"], "testuser");
    assert_eq!(hit["delegate"], TEST_DELEGATE);
    assert_eq!(hit["room"], room_id);

    let fetched = tool_call(
        &client,
        addr,
        "fetch",
        serde_json::json!({"id": format!("post:{post_id}")}),
        Some(&bearer),
    )
    .await;
    assert_eq!(fetched["isError"], false, "{fetched}");
    assert_eq!(fetched["structuredContent"]["actor"], "testuser");
    assert_eq!(fetched["structuredContent"]["delegate"], TEST_DELEGATE);
    assert_eq!(fetched["structuredContent"]["metadata"]["room"], room_id);

    let unauth_room = tool_call(
        &client,
        addr,
        "read_room",
        serde_json::json!({"room_id": room_id}),
        None,
    )
    .await;
    assert_eq!(unauth_room["isError"], true, "{unauth_room}");
    assert!(unauth_room["_meta"]["mcp/www_authenticate"][0].is_string());

    let public_read = tool_call(
        &client,
        addr,
        "read_room",
        serde_json::json!({"room_id": "public"}),
        Some(&bearer),
    )
    .await;
    assert_eq!(public_read["isError"], true, "{public_read}");

    let room = tool_call(
        &client,
        addr,
        "read_room",
        serde_json::json!({"room_id": room_id}),
        Some(&bearer),
    )
    .await;
    assert_eq!(room["isError"], false, "{room}");
    assert_eq!(room["structuredContent"]["room_id"], room_id);
    assert_eq!(room["structuredContent"]["visibility"], "private");
    let members = room["structuredContent"]["members"].as_array().unwrap();
    assert!(
        members.iter().any(|m| m["username"] == "otheruser"),
        "{members:?}"
    );
    let threads = room["structuredContent"]["threads"].as_array().unwrap();
    assert!(
        threads
            .iter()
            .any(|t| t["thread"].as_str() == Some("#private-demo")),
        "{threads:?}"
    );
    let recent = room["structuredContent"]["recent_posts"].as_array().unwrap();
    let recent_hit = recent
        .iter()
        .find(|p| p["post_id"].as_str() == Some(post_id.as_str()))
        .unwrap_or_else(|| panic!("missing recent post: {recent:?}"));
    assert_eq!(recent_hit["actor"], "testuser");
    assert_eq!(recent_hit["delegate"], TEST_DELEGATE);

    let feed = tool_call(
        &client,
        addr,
        "get_feed",
        serde_json::json!({"room_id": room_id, "since": 0}),
        Some(&bearer),
    )
    .await;
    assert_eq!(feed["isError"], false, "{feed}");
    let feed_posts = feed["structuredContent"]["posts"].as_array().unwrap();
    assert!(
        feed_posts
            .iter()
            .any(|p| p["post_id"].as_str() == Some(post_id.as_str())
                && p["actor"] == "testuser"
                && p["delegate"] == TEST_DELEGATE),
        "{feed_posts:?}"
    );

    let since_delegate = tool_call(
        &client,
        addr,
        "get_feed",
        serde_json::json!({"delegate": TEST_DELEGATE, "room_id": room_id}),
        Some(&bearer),
    )
    .await;
    assert_eq!(since_delegate["isError"], false, "{since_delegate}");
    assert_eq!(since_delegate["structuredContent"]["delegate"], TEST_DELEGATE);
    let since_posts = since_delegate["structuredContent"]["posts"]
        .as_array()
        .unwrap();
    assert!(
        since_posts
            .iter()
            .any(|p| p["delegate"] == OTHER_DELEGATE && p["actor"] == "otheruser"),
        "feed since first delegate should include the later otheruser post: {since_posts:?}"
    );
    assert!(
        !since_posts
            .iter()
            .any(|p| p["post_id"].as_str() == Some(post_id.as_str())),
        "cutoff is this delegate's last ingest, so its own post is not new: {since_posts:?}"
    );
}
