use std::time::Duration;

use media_contract::{TrendingCategoryDto, TrendingMediaTypeDto};
use media_integrations::tmdb::{TmdbClient, TmdbConfig, TmdbErrorCode};
use secrecy::SecretString;
use serde_json::json;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path, query_param},
};

fn config(server: &MockServer) -> TmdbConfig {
    TmdbConfig::new(
        format!("{}/3/", server.uri()).parse().unwrap(),
        SecretString::from("test-key"),
        "ru-RU",
        Duration::from_secs(2),
    )
    .unwrap()
}

#[tokio::test]
async fn maps_weekly_trending_and_limits_output_to_ten_items() {
    let server = MockServer::start().await;
    let results = (1..=12)
        .map(|id| {
            let poster_path = match id {
                1 => Some("/film-1.jpg"),
                2 => Some("https://example.invalid/poster.jpg"),
                _ => None,
            };
            let overview = (id == 1).then_some("Описание фильма 1");
            if id % 2 == 0 {
                json!({
                    "id": id,
                    "media_type": "tv",
                    "name": format!("Сериал {id}"),
                    "original_name": format!("Series {id}"),
                    "first_air_date": "2026-07-01",
                    "vote_average": 8.25,
                    "poster_path": poster_path,
                    "overview": overview
                })
            } else {
                json!({
                    "id": id,
                    "media_type": "movie",
                    "title": format!("Фильм {id}"),
                    "original_title": format!("Movie {id}"),
                    "release_date": "2025-12-10",
                    "vote_average": 7.5,
                    "poster_path": poster_path,
                    "overview": overview
                })
            }
        })
        .collect::<Vec<_>>();
    Mock::given(method("GET"))
        .and(path("/3/trending/all/week"))
        .and(query_param("api_key", "test-key"))
        .and(query_param("language", "ru-RU"))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "page": 2,
            "total_pages": 10,
            "total_results": 196,
            "results": results
        })))
        .mount(&server)
        .await;

    let page = TmdbClient::new(config(&server))
        .unwrap()
        .trending(TrendingCategoryDto::All, 2)
        .await
        .unwrap();

    assert_eq!(page.source, "tmdb");
    assert_eq!(page.window, "week");
    assert_eq!(page.page, 2);
    assert_eq!(page.results.len(), 10);
    assert_eq!(page.results[0].title, "Фильм 1");
    assert_eq!(page.results[0].original_title.as_deref(), Some("Movie 1"));
    assert_eq!(page.results[0].year, Some(2025));
    assert_eq!(
        page.results[0].poster_url.as_deref(),
        Some("https://image.tmdb.org/t/p/w780/film-1.jpg")
    );
    assert_eq!(
        page.results[0].overview.as_deref(),
        Some("Описание фильма 1")
    );
    assert_eq!(page.results[1].media_type, TrendingMediaTypeDto::Tv);
    assert_eq!(page.results[1].poster_url, None);
    assert_eq!(page.results[1].overview, None);
    assert_eq!(page.results[9].tmdb_id, 10);
}

#[tokio::test]
async fn classifies_tmdb_authentication_failure() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/3/trending/movie/week"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;

    let error = TmdbClient::new(config(&server))
        .unwrap()
        .trending(TrendingCategoryDto::Movie, 1)
        .await
        .unwrap_err();

    assert_eq!(error.code(), TmdbErrorCode::Unauthorized);
}

#[tokio::test]
async fn rejects_malformed_success_response() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/3/trending/tv/week"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not-json"))
        .mount(&server)
        .await;

    let error = TmdbClient::new(config(&server))
        .unwrap()
        .trending(TrendingCategoryDto::Tv, 1)
        .await
        .unwrap_err();

    assert_eq!(error.code(), TmdbErrorCode::ProviderResponse);
}
