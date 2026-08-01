use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use media_core::{ReleaseMetadataPort, ReleaseMetadataResult, ReleaseQuery};
use media_integrations::tvmaze::{TvmazeClient, TvmazeConfig};
use url::Url;
use wiremock::{
    Mock, MockServer, Request, Respond, ResponseTemplate,
    matchers::{header, method, path, query_param},
};

fn client(server: &MockServer, retries: u8) -> TvmazeClient {
    TvmazeClient::new(
        TvmazeConfig::new(
            Url::parse(&server.uri()).unwrap(),
            Duration::from_secs(2),
            "media-orchestrator-test/1.0".to_owned(),
            retries,
        )
        .unwrap(),
    )
    .unwrap()
}

fn show(id: u64, name: &str, year: &str, status: &str) -> serde_json::Value {
    serde_json::json!({"score": 1.0, "show": {
        "id": id, "name": name, "premiered": year, "status": status
    }})
}

#[tokio::test]
async fn ambiguous_search_returns_choices_without_fetching_episodes() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search/shows"))
        .and(query_param("q", "The Office"))
        .and(header("user-agent", "media-orchestrator-test/1.0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            show(1, "The Office", "2005-03-24", "Ended"),
            show(2, "The Office", "2024-01-01", "Ended")
        ])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(path("/shows/1/episodes"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(path("/shows/2/episodes"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let result = client(&server, 0)
        .query(&ReleaseQuery::new("The Office", None, None).unwrap())
        .await
        .unwrap();

    match result {
        ReleaseMetadataResult::ChoiceNeeded {
            source, candidates, ..
        } => {
            assert_eq!(source, "tvmaze");
            assert_eq!(candidates.len(), 2);
        }
        _ => panic!("expected explicit choice"),
    }
}

#[tokio::test]
async fn matched_show_reports_counts_next_episode_and_full_schedule() {
    let server = MockServer::start().await;
    Mock::given(path("/search/shows"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!([show(
                10,
                "Severance",
                "2022-02-18",
                "Running"
            )])),
        )
        .mount(&server)
        .await;
    Mock::given(path("/shows/10/episodes"))
        .and(query_param("specials", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"id": 100, "name": "Good News About Hell", "season": 1, "number": 1, "airdate": "2022-02-18", "airstamp": "2022-02-18T14:00:00+00:00"},
            {"id": 200, "name": "Future", "season": 3, "number": 1, "airdate": "2099-01-01", "airstamp": "2099-01-01T14:00:00+00:00"}
        ]))).expect(1).mount(&server).await;

    let result = client(&server, 0)
        .query(&ReleaseQuery::new("Severance", None, Some(2022)).unwrap())
        .await
        .unwrap();

    match result {
        ReleaseMetadataResult::Matched {
            released_episodes,
            expected_episodes,
            next_episode,
            schedule,
            ..
        } => {
            assert_eq!(released_episodes, 1);
            assert_eq!(expected_episodes, Some(2));
            assert_eq!(next_episode.unwrap().source_id, 200);
            assert_eq!(schedule.len(), 2);
        }
        _ => panic!("expected match"),
    }
}

#[derive(Clone)]
struct TransientThenOk(Arc<AtomicUsize>);

impl Respond for TransientThenOk {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        let attempt = self.0.fetch_add(1, Ordering::SeqCst);
        if attempt == 0 {
            ResponseTemplate::new(429).insert_header("retry-after", "0")
        } else {
            ResponseTemplate::new(200).set_body_json(serde_json::json!([show(
                10,
                "Severance",
                "2022-02-18",
                "Ended"
            )]))
        }
    }
}

#[tokio::test]
async fn retries_429_only_within_configured_bound() {
    let server = MockServer::start().await;
    let attempts = Arc::new(AtomicUsize::new(0));
    Mock::given(path("/search/shows"))
        .respond_with(TransientThenOk(attempts.clone()))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(path("/shows/10/episodes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&server)
        .await;

    client(&server, 1)
        .query(&ReleaseQuery::new("Severance", None, Some(2022)).unwrap())
        .await
        .unwrap();

    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}
