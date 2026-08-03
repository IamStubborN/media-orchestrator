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

#[tokio::test]
async fn maps_tv_details_with_provider_metadata_and_safe_urls() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/3/tv/42"))
        .and(query_param("api_key", "test-key"))
        .and(query_param("language", "ru-RU"))
        .and(query_param("append_to_response", "external_ids,videos"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 42,
            "name": "Тестовый сериал",
            "original_name": "Test Series",
            "first_air_date": "2026-02-03",
            "vote_average": 8.26,
            "poster_path": "/series.jpg",
            "overview": "Описание сериала",
            "production_countries": [{"name": "United States of America"}],
            "origin_country": ["US"],
            "genres": [{"name": "Драма"}, {"name": "Фантастика"}],
            "status": "Returning Series",
            "number_of_seasons": 3,
            "number_of_episodes": 24,
            "next_episode_to_air": {
                "season_number": 4,
                "episode_number": 1,
                "air_date": "2026-08-11"
            },
            "external_ids": {"imdb_id": "tt1234567"},
            "videos": {"results": [
                {"key": "abc_123-xyz", "site": "YouTube", "type": "Trailer", "official": true},
                {"key": "ignored", "site": "Vimeo", "type": "Trailer", "official": true}
            ]}
        })))
        .mount(&server)
        .await;

    let details = TmdbClient::new(config(&server))
        .unwrap()
        .details(42, TrendingMediaTypeDto::Tv)
        .await
        .unwrap();

    assert_eq!(details.tmdb_id, 42);
    assert_eq!(details.title, "Тестовый сериал");
    assert_eq!(details.original_title.as_deref(), Some("Test Series"));
    assert_eq!(details.release_date.as_deref(), Some("2026-02-03"));
    assert_eq!(details.year, Some(2026));
    assert_eq!(details.rating, Some(8.3));
    assert_eq!(
        details.poster_url.as_deref(),
        Some("https://image.tmdb.org/t/p/w780/series.jpg")
    );
    assert_eq!(details.countries, vec!["United States of America"]);
    assert_eq!(details.genres, vec!["Драма", "Фантастика"]);
    assert_eq!(details.season_count, Some(3));
    assert_eq!(details.episode_count, Some(24));
    let next_episode = details.next_episode.as_ref().unwrap();
    assert_eq!(next_episode.season, 4);
    assert_eq!(next_episode.episode, 1);
    assert_eq!(next_episode.air_date, "2026-08-11");
    assert_eq!(
        details.tmdb_url.as_deref(),
        Some("https://www.themoviedb.org/tv/42")
    );
    assert_eq!(
        details.imdb_url.as_deref(),
        Some("https://www.imdb.com/title/tt1234567/")
    );
    assert_eq!(
        details.trailer_url.as_deref(),
        Some("https://www.youtube.com/watch?v=abc_123-xyz")
    );
}

#[tokio::test]
async fn maps_similar_results_without_provider_media_type_and_limits_to_ten() {
    let server = MockServer::start().await;
    let results = (1..=12)
        .map(|id| {
            json!({
                "id": id,
                "name": format!("Похожий сериал {id}"),
                "original_name": format!("Similar Series {id}"),
                "first_air_date": "2025-01-01",
                "vote_average": 7.1
            })
        })
        .collect::<Vec<_>>();
    Mock::given(method("GET"))
        .and(path("/3/tv/42/recommendations"))
        .and(query_param("api_key", "test-key"))
        .and(query_param("language", "ru-RU"))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "page": 2,
            "total_pages": 4,
            "total_results": 37,
            "results": results
        })))
        .mount(&server)
        .await;

    let page = TmdbClient::new(config(&server))
        .unwrap()
        .similar(42, TrendingMediaTypeDto::Tv, 2)
        .await
        .unwrap();

    assert_eq!(page.tmdb_id, 42);
    assert_eq!(page.media_type, TrendingMediaTypeDto::Tv);
    assert_eq!(page.page, 2);
    assert_eq!(page.total_pages, 4);
    assert_eq!(page.total_results, 37);
    assert_eq!(page.results.len(), 10);
    assert_eq!(page.results[0].title, "Похожий сериал 1");
    assert_eq!(page.results[0].media_type, TrendingMediaTypeDto::Tv);
    assert_eq!(page.results[9].tmdb_id, 10);
}

#[tokio::test]
async fn rejects_invalid_details_and_similar_requests() {
    let server = MockServer::start().await;
    let client = TmdbClient::new(config(&server)).unwrap();

    assert_eq!(
        client
            .details(0, TrendingMediaTypeDto::Movie)
            .await
            .unwrap_err()
            .code(),
        TmdbErrorCode::InvalidRequest
    );
    assert_eq!(
        client
            .similar(42, TrendingMediaTypeDto::Movie, 0)
            .await
            .unwrap_err()
            .code(),
        TmdbErrorCode::InvalidRequest
    );
}
