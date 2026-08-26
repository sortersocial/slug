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
        .contains("ask the human"));

    let listed = mcp_call(&client, addr, "tools/list", serde_json::json!({}), None).await;
    let tools = listed["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    for expected in [
        "search",
        "fetch",
        "list_threads",
        "get_thread",
        "get_rank",
        "get_item",
        "get_pair",
        "check_sorter",
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
    let search = tools.iter().find(|t| t["name"] == "search").unwrap();
    assert_eq!(search["annotations"]["readOnlyHint"], true);
    assert_eq!(search["securitySchemes"][0]["type"], "noauth");
}

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

    let posted = tool_call(
        &client,
        addr,
        "post_sorter",
        serde_json::json!({"thread_tag": "mcp-demo", "text": doc}),
        Some(&bearer),
    )
    .await;
    assert_eq!(posted["isError"], false, "{posted}");
    let post_id = posted["structuredContent"]["PostOk"]["post_id"]
        .as_str()
        .unwrap()
        .to_string();

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
