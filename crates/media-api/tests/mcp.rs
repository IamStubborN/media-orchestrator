mod support;

use axum::{body::Body, body::to_bytes, http::Request};
use media_api::router;
use media_core::{PRIMARY_CLIENT_ID, PRIMARY_USER_ID, Actor, ClientRole};
use serde_json::Value;
use tower::ServiceExt;

use support::{FakeClientStore, FakeReadiness, VALID_TOKEN, state};

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
async fn authenticated_mcp_session_lists_only_the_admin_tools() {
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
    let names: Vec<&str> = body["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"media_jobs_list"));
    assert!(names.contains(&"media_job_cancel"));
    assert!(names.contains(&"media_tracking_check"));
    assert!(names.contains(&"media_search"));
    assert!(names.contains(&"plex_search"));
    assert!(names.contains(&"plex_now_playing"));
    assert!(names.contains(&"qbittorrent_list"));
    assert!(names.contains(&"qbittorrent_control"));
    assert!(names.contains(&"media_file_inspect"));
    assert!(names.contains(&"media_infrastructure_status"));
    assert!(names.contains(&"media_destructive_prepare"));
    assert!(names.contains(&"media_destructive_confirm"));
    assert_eq!(names.len(), 20);

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
