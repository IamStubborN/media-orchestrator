mod support;

use axum::{body::Body, body::to_bytes, http::Request};
use media_api::router;
use media_core::{PRIMARY_CLIENT_ID, PRIMARY_USER_ID, Actor, ClientRole};
use serde_json::Value;
use tower::ServiceExt;

use support::{FakeClientStore, FakeReadiness, VALID_TOKEN, state};

fn capability_manifest_tool_names() -> Vec<String> {
    let manifest: Value =
        serde_json::from_str(include_str!("../../../config/media-capabilities.json"))
            .expect("media capability manifest must be valid JSON");
    let mut names: Vec<String> = manifest["tools"]
        .as_array()
        .expect("media capability manifest must contain a tools array")
        .iter()
        .map(|name| {
            name.as_str()
                .expect("media capability names must be strings")
                .to_owned()
        })
        .collect();
    names.sort_unstable();
    names
}

fn mcp_request(body: &'static str) -> Request<Body> {
    Request::post("/internal/mcp")
        .header("authorization", format!("Bearer {VALID_TOKEN}"))
        .header("host", "media-service")
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .body(Body::from(body))
        .unwrap()
}

#[tokio::test]
async fn legacy_mcp_session_lists_the_complete_media_toolset() {
    let app = router(state(
        FakeClientStore::new([(
            VALID_TOKEN,
            Actor::new(PRIMARY_CLIENT_ID, Some(PRIMARY_USER_ID), ClientRole::Hermes).unwrap(),
        )]),
        FakeReadiness::ready(),
    ));

    let initialized = app
        .clone()
        .oneshot(mcp_request(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}"#,
        ))
        .await
        .unwrap();
    assert_eq!(initialized.status(), 200);
    let body: Value =
        serde_json::from_slice(&to_bytes(initialized.into_body(), usize::MAX).await.unwrap())
            .unwrap();
    assert_eq!(body["result"]["protocolVersion"], "2025-03-26");

    let listed = app
        .clone()
        .oneshot(
            Request::post("/internal/mcp")
                .header("authorization", format!("Bearer {VALID_TOKEN}"))
                .header("host", "media-service")
                .header("content-type", "application/json")
                .header("accept", "application/json, text/event-stream")
                .header("mcp-protocol-version", "2025-03-26")
                .body(Body::from(
                    r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(listed.status(), 200);
    let body: Value =
        serde_json::from_slice(&to_bytes(listed.into_body(), usize::MAX).await.unwrap()).unwrap();
    let names: Vec<String> = body["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(names, capability_manifest_tool_names());
    for tool in body["result"]["tools"].as_array().unwrap() {
        assert!(
            tool["outputSchema"].is_object(),
            "missing output schema: {tool}"
        );
        assert!(
            tool["annotations"].is_object(),
            "missing annotations: {tool}"
        );
    }
    let retry = body["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "media_job_retry")
        .unwrap();
    assert_eq!(retry["annotations"]["idempotentHint"], false);

    let called = app
        .oneshot(
            Request::post("/internal/mcp")
                .header("authorization", format!("Bearer {VALID_TOKEN}"))
                .header("host", "media-service")
                .header("content-type", "application/json")
                .header("accept", "application/json, text/event-stream")
                .header("mcp-protocol-version", "2025-03-26")
                .body(Body::from(
                    r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"media_queue_status","arguments":{}}}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(called.status(), 200);
    let body: Value =
        serde_json::from_slice(&to_bytes(called.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(body["result"]["isError"], false);
    assert_eq!(body["result"]["structuredContent"]["queued"], 0);
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let queue: Value = serde_json::from_str(text).unwrap();
    assert_eq!(queue["queued"], 0);
}

#[tokio::test]
async fn stateless_mcp_2026_lists_tools_without_initialize_or_session() {
    let app = router(state(
        FakeClientStore::new([(
            VALID_TOKEN,
            Actor::new(PRIMARY_CLIENT_ID, Some(PRIMARY_USER_ID), ClientRole::Hermes).unwrap(),
        )]),
        FakeReadiness::ready(),
    ));
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientInfo":{"name":"media-api-test","version":"1.0"},"io.modelcontextprotocol/clientCapabilities":{}}}}"#;
    let response = app
        .oneshot(
            Request::post("/internal/mcp")
                .header("authorization", format!("Bearer {VALID_TOKEN}"))
                .header("host", "media-service")
                .header("content-type", "application/json")
                .header("accept", "application/json, text/event-stream")
                .header("mcp-protocol-version", "2026-07-28")
                .header("mcp-method", "tools/list")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
    assert!(response.headers().get("mcp-session-id").is_none());
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    let names: Vec<String> = body["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(names, capability_manifest_tool_names());
    assert_eq!(body["result"]["ttlMs"], 0);
    assert_eq!(body["result"]["cacheScope"], "public");
}

#[tokio::test]
async fn stateless_mcp_2026_discovers_server_without_initialize_or_session() {
    let app = router(state(
        FakeClientStore::new([(
            VALID_TOKEN,
            Actor::new(PRIMARY_CLIENT_ID, Some(PRIMARY_USER_ID), ClientRole::Hermes).unwrap(),
        )]),
        FakeReadiness::ready(),
    ));
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientInfo":{"name":"media-api-test","version":"1.0"},"io.modelcontextprotocol/clientCapabilities":{}}}}"#;
    let response = app
        .oneshot(
            Request::post("/internal/mcp")
                .header("authorization", format!("Bearer {VALID_TOKEN}"))
                .header("host", "media-service")
                .header("content-type", "application/json")
                .header("accept", "application/json, text/event-stream")
                .header("mcp-protocol-version", "2026-07-28")
                .header("mcp-method", "server/discover")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
    assert!(response.headers().get("mcp-session-id").is_none());
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(body["result"]["resultType"], "complete");
    assert!(
        body["result"]["supportedVersions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|version| version == "2026-07-28")
    );
    assert!(body["result"]["capabilities"]["tools"].is_object());
    assert_eq!(body["result"]["ttlMs"], 0);
    assert_eq!(body["result"]["cacheScope"], "private");
}

#[tokio::test]
async fn rezka_session_refresh_never_echoes_the_credential_request() {
    let app = router(state(
        FakeClientStore::new([(
            VALID_TOKEN,
            Actor::new(PRIMARY_CLIENT_ID, Some(PRIMARY_USER_ID), ClientRole::Hermes).unwrap(),
        )]),
        FakeReadiness::ready(),
    ));
    let secret_request_id = "approved-request-secret-42";
    let body = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"media_rezka_session_refresh","arguments":{{"credential_request_id":"{secret_request_id}"}}}}}}"#
    );
    let response = app
        .oneshot(
            Request::post("/internal/mcp")
                .header("authorization", format!("Bearer {VALID_TOKEN}"))
                .header("host", "media-service")
                .header("content-type", "application/json")
                .header("accept", "application/json, text/event-stream")
                .header("mcp-protocol-version", "2025-03-26")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
    let response = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let response = String::from_utf8(response.to_vec()).unwrap();
    assert!(!response.contains(secret_request_id));
}

#[tokio::test]
async fn stateless_mcp_2026_rejects_missing_request_metadata() {
    let app = router(state(
        FakeClientStore::new([(
            VALID_TOKEN,
            Actor::new(PRIMARY_CLIENT_ID, Some(PRIMARY_USER_ID), ClientRole::Hermes).unwrap(),
        )]),
        FakeReadiness::ready(),
    ));
    let response = app
        .oneshot(
            Request::post("/internal/mcp")
                .header("authorization", format!("Bearer {VALID_TOKEN}"))
                .header("host", "media-service")
                .header("content-type", "application/json")
                .header("accept", "application/json, text/event-stream")
                .header("mcp-protocol-version", "2026-07-28")
                .header("mcp-method", "tools/list")
                .body(Body::from(
                    r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), 400);
    assert!(response.headers().get("mcp-session-id").is_none());
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(body["error"]["code"], -32602);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("request _meta")
    );
}

#[tokio::test]
async fn stateless_mcp_2026_rejects_mismatched_routing_header() {
    let app = router(state(
        FakeClientStore::new([(
            VALID_TOKEN,
            Actor::new(PRIMARY_CLIENT_ID, Some(PRIMARY_USER_ID), ClientRole::Hermes).unwrap(),
        )]),
        FakeReadiness::ready(),
    ));
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientInfo":{"name":"media-api-test","version":"1.0"},"io.modelcontextprotocol/clientCapabilities":{}}}}"#;
    let response = app
        .oneshot(
            Request::post("/internal/mcp")
                .header("authorization", format!("Bearer {VALID_TOKEN}"))
                .header("host", "media-service")
                .header("content-type", "application/json")
                .header("accept", "application/json, text/event-stream")
                .header("mcp-protocol-version", "2026-07-28")
                .header("mcp-method", "tools/call")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), 400);
    assert!(response.headers().get("mcp-session-id").is_none());
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(body["error"]["code"], -32020);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Mcp-Method")
    );
}

#[tokio::test]
async fn stateless_mcp_2026_requires_tool_name_routing_header() {
    let app = router(state(
        FakeClientStore::new([(
            VALID_TOKEN,
            Actor::new(PRIMARY_CLIENT_ID, Some(PRIMARY_USER_ID), ClientRole::Hermes).unwrap(),
        )]),
        FakeReadiness::ready(),
    ));
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"media_queue_status","arguments":{},"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientInfo":{"name":"media-api-test","version":"1.0"},"io.modelcontextprotocol/clientCapabilities":{}}}}"#;
    let response = app
        .oneshot(
            Request::post("/internal/mcp")
                .header("authorization", format!("Bearer {VALID_TOKEN}"))
                .header("host", "media-service")
                .header("content-type", "application/json")
                .header("accept", "application/json, text/event-stream")
                .header("mcp-protocol-version", "2026-07-28")
                .header("mcp-method", "tools/call")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), 400);
    assert!(response.headers().get("mcp-session-id").is_none());
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(body["error"]["code"], -32020);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Mcp-Name")
    );
}
