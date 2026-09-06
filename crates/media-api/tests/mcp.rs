mod support;

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use axum::{body::Body, body::to_bytes, http::Request};
use media_api::router;
use media_core::{
    PRIMARY_CLIENT_ID, PRIMARY_USER_ID, Actor, ClientRole, NewTrackingSubscription, OperationKey,
    PortError, TrackingDownloadPatch, TrackingId, TrackingStore, TrackingSubscription, UserId,
    SECONDARY_CLIENT_ID, SECONDARY_USER_ID,
};
use serde_json::Value;
use tower::ServiceExt;

use support::{FakeClientStore, FakeReadiness, SECONDARY_TOKEN, VALID_TOKEN, state};

#[derive(Default)]
struct IdempotentTrackingStore {
    operations: Mutex<HashMap<OperationKey, TrackingSubscription>>,
}

#[async_trait::async_trait]
impl TrackingStore for IdempotentTrackingStore {
    async fn add(
        &self,
        operation: OperationKey,
        value: NewTrackingSubscription,
    ) -> Result<TrackingSubscription, PortError> {
        let mut operations = self.operations.lock().unwrap();
        if let Some(existing) = operations.get(&operation) {
            return Ok(existing.clone());
        }
        let value = value.into_persisted();
        operations.insert(operation, value.clone());
        Ok(value)
    }

    async fn list_visible(&self, _: UserId) -> Result<Vec<TrackingSubscription>, PortError> {
        unreachable!("tracking-create regression does not list subscriptions")
    }

    async fn patch_download_visible(
        &self,
        _: TrackingId,
        _: UserId,
        _: TrackingDownloadPatch,
    ) -> Result<Option<TrackingSubscription>, PortError> {
        unreachable!("tracking-create regression does not patch subscriptions")
    }

    async fn remove_visible(
        &self,
        _: OperationKey,
        _: TrackingId,
        _: UserId,
    ) -> Result<Option<TrackingSubscription>, PortError> {
        unreachable!("tracking-create regression does not remove subscriptions")
    }
}

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

fn tracking_create_request(token: &str, id: u32) -> Request<Body> {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": {
            "name": "media_tracking_create",
            "arguments": {
                "provider": "rezka",
                "title": "Shared Show",
                "translation": "Studio Dub",
                "known_episodes": [{"season": 1, "episode": 4}],
                "scope": "personal",
                "series_ongoing": true
            },
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientInfo": {
                    "name": "tracking-idempotency-test",
                    "version": "1.0"
                },
                "io.modelcontextprotocol/clientCapabilities": {}
            }
        }
    });
    Request::post("/internal/mcp")
        .header("authorization", format!("Bearer {token}"))
        .header("host", "media-service")
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("mcp-protocol-version", "2026-07-28")
        .header("mcp-method", "tools/call")
        .header("mcp-name", "media_tracking_create")
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[tokio::test]
async fn identical_tracking_create_payloads_are_idempotent_per_owner() {
    let primary = Actor::new(PRIMARY_CLIENT_ID, Some(PRIMARY_USER_ID), ClientRole::Hermes).unwrap();
    let secondary = Actor::new(
        SECONDARY_CLIENT_ID,
        Some(SECONDARY_USER_ID),
        ClientRole::Hermes,
    )
    .unwrap();
    let app = router(
        state(
            FakeClientStore::new([(VALID_TOKEN, primary), (SECONDARY_TOKEN, secondary)]),
            FakeReadiness::ready(),
        )
        .with_tracking(Arc::new(media_core::TrackingApplication::new(Arc::new(
            IdempotentTrackingStore::default(),
        )))),
    );

    let first = app
        .clone()
        .oneshot(tracking_create_request(VALID_TOKEN, 1))
        .await
        .unwrap();
    let second = app
        .clone()
        .oneshot(tracking_create_request(SECONDARY_TOKEN, 2))
        .await
        .unwrap();
    let replay = app
        .oneshot(tracking_create_request(VALID_TOKEN, 3))
        .await
        .unwrap();
    let first: Value =
        serde_json::from_slice(&to_bytes(first.into_body(), usize::MAX).await.unwrap()).unwrap();
    let second: Value =
        serde_json::from_slice(&to_bytes(second.into_body(), usize::MAX).await.unwrap()).unwrap();
    let replay: Value =
        serde_json::from_slice(&to_bytes(replay.into_body(), usize::MAX).await.unwrap()).unwrap();

    assert_eq!(first["result"]["isError"], false, "{first}");
    assert_eq!(second["result"]["isError"], false, "{second}");
    assert_ne!(
        first["result"]["structuredContent"]["id"],
        second["result"]["structuredContent"]["id"]
    );
    assert_eq!(
        first["result"]["structuredContent"]["id"],
        replay["result"]["structuredContent"]["id"]
    );
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
    for mutation in ["media_job_cancel", "media_job_retry"] {
        let tool = body["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == mutation)
            .unwrap();
        assert!(
            tool["inputSchema"]["properties"]["expected_lifecycle_cycle"].is_object(),
            "{mutation} must expose the lifecycle fence input"
        );
    }

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
    assert_eq!(body["result"]["ttlMs"], 300_000);
    assert_eq!(body["result"]["cacheScope"], "public");

    let search = body["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "media_search")
        .unwrap();
    assert_eq!(search["annotations"]["readOnlyHint"], false);
    assert_eq!(search["annotations"]["idempotentHint"], false);
    let prepare = body["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "media_destructive_prepare")
        .unwrap();
    assert_eq!(prepare["annotations"]["readOnlyHint"], false);
    assert_eq!(prepare["annotations"]["idempotentHint"], false);
    assert_eq!(search["inputSchema"]["properties"]["tmdb_id"]["minimum"], 1);
    assert_eq!(
        search["inputSchema"]["properties"]["tmdb_id"]["format"],
        "uint64"
    );

    for name in ["media_jobs_list", "media_tracking_list", "plex_recent"] {
        let tool = body["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap();
        assert_eq!(tool["inputSchema"]["properties"]["limit"]["minimum"], 1);
        assert_eq!(tool["inputSchema"]["properties"]["limit"]["maximum"], 50);
    }
    for (name, property) in [
        ("media_jobs_list", "jobs"),
        ("media_tracking_list", "tracking"),
        ("plex_recent", "items"),
        ("media_storage_status", "roots"),
    ] {
        let tool = body["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap();
        assert!(
            tool["outputSchema"]["properties"][property].is_object(),
            "{name} must publish a typed {property} output"
        );
    }

    for (name, required_input, required_output) in [
        ("media_best", "ranking", "results"),
        ("media_premieres", "feed", "results"),
        ("media_genres", "media_type", "genres"),
        ("media_discover", "genre_id", "results"),
    ] {
        let tool = body["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap_or_else(|| panic!("missing discovery tool {name}"));
        assert!(
            tool["inputSchema"]["properties"][required_input].is_object(),
            "{name} must publish typed input {required_input}"
        );
        assert!(
            tool["outputSchema"]["properties"][required_output].is_object(),
            "{name} must publish typed output {required_output}"
        );
        if name != "media_genres" {
            assert_eq!(
                tool["inputSchema"]["properties"]["page"]["minimum"], 1,
                "{name} must publish a positive page input"
            );
            assert_eq!(
                tool["inputSchema"]["properties"]["page"]["format"], "uint32",
                "{name} must preserve the page width"
            );
            assert_eq!(
                tool["outputSchema"]["properties"]["page"]["minimum"], 1,
                "{name} must publish a positive page output"
            );
            assert_eq!(
                tool["outputSchema"]["properties"]["page"]["format"], "uint32",
                "{name} must preserve the output page width"
            );
            assert_eq!(
                tool["outputSchema"]["properties"]["results"]["maxItems"], 10,
                "{name} must publish the list-first result bound"
            );
            assert_eq!(
                tool["outputSchema"]["$defs"]["DiscoveryItemOutput"]["properties"]["tmdb_id"]["minimum"],
                1,
                "{name} must publish positive TMDB result identifiers"
            );
            assert_eq!(
                tool["outputSchema"]["$defs"]["DiscoveryItemOutput"]["properties"]["tmdb_id"]["format"],
                "uint64",
                "{name} must preserve the TMDB identifier width"
            );
            assert_eq!(
                tool["outputSchema"]["$defs"]["DiscoveryItemOutput"]["properties"]["rating"]["format"],
                "float",
                "{name} must preserve the rating width"
            );
        }
    }

    let trending = body["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "media_trending")
        .unwrap();
    assert_eq!(trending["inputSchema"]["properties"]["page"]["minimum"], 1);
    assert_eq!(
        trending["inputSchema"]["properties"]["page"]["format"],
        "uint32"
    );

    for name in ["media_jobs_list", "media_tracking_list"] {
        let tool = body["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap();
        let item = if name == "media_jobs_list" {
            &tool["outputSchema"]["$defs"]["JobListItemOutput"]
        } else {
            &tool["outputSchema"]["$defs"]["TrackingListItemOutput"]
        };
        assert!(item["properties"]["poster_url"].is_object());
        if name == "media_tracking_list" {
            assert!(item["properties"]["status_reason"].is_object());
            assert!(item["properties"]["last_error"].is_object());
            assert!(item["properties"]["pending_episodes"].is_object());
            assert!(item["properties"]["pending_age_seconds"].is_object());
        }
        if name == "media_jobs_list" {
            assert!(item["properties"]["library_title"].is_object());
            assert!(item["properties"]["translation"].is_object());
        }
    }
    let tracking_create = body["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "media_tracking_create")
        .unwrap();
    assert!(tracking_create["inputSchema"]["properties"]["poster_url"].is_object());

    let serialized = serde_json::to_vec(&body["result"]["tools"]).unwrap();
    if let Ok(path) = std::env::var("MCP_SCHEMA_SNAPSHOT") {
        std::fs::write(path, &serialized).unwrap();
    }
    assert!(
        serialized.len() <= 34_000,
        "tool discovery schema grew unexpectedly: {} bytes",
        serialized.len()
    );
    for tool in body["result"]["tools"].as_array().unwrap() {
        let tool_size = serde_json::to_vec(tool).unwrap().len();
        assert!(
            tool_size <= 2_600,
            "{} schema grew unexpectedly: {tool_size} bytes",
            tool["name"]
        );
    }
}

#[tokio::test]
async fn discovery_tools_reject_zero_pages_at_the_mcp_boundary() {
    let app = router(state(
        FakeClientStore::new([(
            VALID_TOKEN,
            Actor::new(PRIMARY_CLIENT_ID, Some(PRIMARY_USER_ID), ClientRole::Hermes).unwrap(),
        )]),
        FakeReadiness::ready(),
    ));

    for (request_id, name, arguments) in [
        (1, "media_trending", serde_json::json!({"page": 0})),
        (
            2,
            "media_best",
            serde_json::json!({"media_type": "movie", "page": 0}),
        ),
        (
            3,
            "media_premieres",
            serde_json::json!({"media_type": "movie", "page": 0}),
        ),
        (
            4,
            "media_discover",
            serde_json::json!({"media_type": "movie", "genre_id": 28, "page": 0}),
        ),
    ] {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "tools/call",
            "params": {
                "name": name,
                "arguments": arguments,
                "_meta": {
                    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                    "io.modelcontextprotocol/clientInfo": {
                        "name": "media-api-test",
                        "version": "1.0"
                    },
                    "io.modelcontextprotocol/clientCapabilities": {}
                }
            }
        });
        let response = app
            .clone()
            .oneshot(
                Request::post("/internal/mcp")
                    .header("authorization", format!("Bearer {VALID_TOKEN}"))
                    .header("host", "media-service")
                    .header("content-type", "application/json")
                    .header("accept", "application/json, text/event-stream")
                    .header("mcp-protocol-version", "2026-07-28")
                    .header("mcp-method", "tools/call")
                    .header("mcp-tool-name", name)
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            response.status(),
            400,
            "{name} accepted page zero at the MCP boundary"
        );
    }
}

#[tokio::test]
async fn stateless_mcp_2026_returns_each_tool_result_once() {
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
                .header("mcp-name", "media_queue_status")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    if let Ok(path) = std::env::var("MCP_RESULT_SNAPSHOT") {
        std::fs::write(path, serde_json::to_vec(&body).unwrap()).unwrap();
    }
    assert_eq!(body["result"]["isError"], false);
    assert_eq!(body["result"]["structuredContent"]["queued"], 0);
    assert_eq!(body["result"]["content"], serde_json::json!([]));
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
