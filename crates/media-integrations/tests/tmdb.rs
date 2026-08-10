use std::time::Duration;

use media_contract::{BestRankingDto, PremiereFeedDto, TrendingCategoryDto, TrendingMediaTypeDto};
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
        .and(query_param("page", "1"))
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
    assert_eq!(page.total_pages, 20);
    assert_eq!(page.results.len(), 2);
    assert_eq!(page.results[0].tmdb_id, 11);
    assert_eq!(page.results[1].tmdb_id, 12);
}

#[tokio::test]
async fn finds_first_matching_title_for_a_release_poster() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/3/search/tv"))
        .and(query_param("api_key", "test-key"))
        .and(query_param("language", "ru-RU"))
        .and(query_param("query", "One Piece"))
        .and(query_param("page", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "page": 1,
            "total_pages": 1,
            "total_results": 2,
            "results": [{
                "id": 999,
                "name": "One Piece Live Event",
                "original_name": "One Piece Live Event",
                "first_air_date": "2026-01-01",
                "poster_path": "/wrong.jpg"
            }, {
                "id": 37854,
                "name": "Ван-Пис",
                "original_name": "One Piece",
                "first_air_date": "1999-10-20",
                "poster_path": "/one-piece.jpg"
            }]
        })))
        .mount(&server)
        .await;

    let item = TmdbClient::new(config(&server))
        .unwrap()
        .find("One Piece", TrendingMediaTypeDto::Tv)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(item.tmdb_id, 37854);
    assert_eq!(item.title, "Ван-Пис");
    assert_eq!(
        item.poster_url.as_deref(),
        Some("https://image.tmdb.org/t/p/w780/one-piece.jpg")
    );
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
        .and(query_param("page", "1"))
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
    assert_eq!(page.total_pages, 4);
    assert_eq!(page.results.len(), 2);
    assert_eq!(page.results[0].title, "Похожий сериал 11");
    assert_eq!(page.results[0].media_type, TrendingMediaTypeDto::Tv);
    assert_eq!(page.results[1].tmdb_id, 12);
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

#[tokio::test]
async fn lists_top_rated_movies_and_limits_output_to_ten_items() {
    let server = MockServer::start().await;
    let results = (1..=12)
        .map(|id| {
            json!({
                "id": id,
                "title": format!("Best Movie {id}"),
                "original_title": format!("Original Movie {id}"),
                "release_date": "2026-06-01",
                "vote_average": 8.4,
                "poster_path": format!("/best-{id}.jpg")
            })
        })
        .collect::<Vec<_>>();
    Mock::given(method("GET"))
        .and(path("/3/movie/top_rated"))
        .and(query_param("api_key", "test-key"))
        .and(query_param("language", "ru-RU"))
        .and(query_param("page", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "page": 2,
            "total_pages": 7,
            "total_results": 68,
            "results": results
        })))
        .mount(&server)
        .await;

    let page = TmdbClient::new(config(&server))
        .unwrap()
        .best(TrendingMediaTypeDto::Movie, BestRankingDto::TopRated, 2)
        .await
        .unwrap();

    assert_eq!(page.ranking, BestRankingDto::TopRated);
    assert_eq!(page.media_type, TrendingMediaTypeDto::Movie);
    assert_eq!(page.page, 2);
    assert_eq!(page.total_pages, 7);
    assert_eq!(page.results.len(), 2);
    assert_eq!(page.results[0].tmdb_id, 11);
    assert_eq!(page.results[1].tmdb_id, 12);
}

#[tokio::test]
async fn drops_discovery_results_without_a_positive_tmdb_id() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/3/movie/popular"))
        .and(query_param("page", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "total_results": 2,
            "results": [
                {"id": 0, "title": "Invalid provider result"},
                {"id": 42, "title": "Valid provider result"}
            ]
        })))
        .mount(&server)
        .await;

    let page = TmdbClient::new(config(&server))
        .unwrap()
        .best(TrendingMediaTypeDto::Movie, BestRankingDto::Popular, 1)
        .await
        .unwrap();

    assert_eq!(page.results.len(), 1);
    assert_eq!(page.results[0].tmdb_id, 42);
}

#[tokio::test]
async fn lists_tv_premieres_from_on_the_air_feed() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/3/tv/on_the_air"))
        .and(query_param("api_key", "test-key"))
        .and(query_param("language", "ru-RU"))
        .and(query_param("page", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "page": 1,
            "total_pages": 3,
            "total_results": 22,
            "results": [{
                "id": 42,
                "name": "Новый сериал",
                "original_name": "New Series",
                "first_air_date": "2026-08-10",
                "vote_average": 7.8,
                "poster_path": "/new-series.jpg",
                "overview": "Описание"
            }]
        })))
        .mount(&server)
        .await;

    let page = TmdbClient::new(config(&server))
        .unwrap()
        .premieres(TrendingMediaTypeDto::Tv, PremiereFeedDto::OnTheAir, 1)
        .await
        .unwrap();

    assert_eq!(page.feed, PremiereFeedDto::OnTheAir);
    assert_eq!(page.media_type, TrendingMediaTypeDto::Tv);
    assert_eq!(page.results[0].tmdb_id, 42);
    assert_eq!(page.results[0].title, "Новый сериал");
}

#[tokio::test]
async fn lists_localized_movie_genres() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/3/genre/movie/list"))
        .and(query_param("api_key", "test-key"))
        .and(query_param("language", "ru-RU"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "genres": [
                {"id": 28, "name": "Боевик"},
                {"id": 35, "name": "Комедия"}
            ]
        })))
        .mount(&server)
        .await;

    let genres = TmdbClient::new(config(&server))
        .unwrap()
        .genres(TrendingMediaTypeDto::Movie)
        .await
        .unwrap();

    assert_eq!(genres.media_type, TrendingMediaTypeDto::Movie);
    assert_eq!(genres.genres.len(), 2);
    assert_eq!(genres.genres[0].id, 28);
    assert_eq!(genres.genres[0].name, "Боевик");
}

#[tokio::test]
async fn discovers_tv_by_genre_and_popularity() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/3/discover/tv"))
        .and(query_param("api_key", "test-key"))
        .and(query_param("language", "ru-RU"))
        .and(query_param("with_genres", "18"))
        .and(query_param("sort_by", "popularity.desc"))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "page": 3,
            "total_pages": 9,
            "total_results": 82,
            "results": [{
                "id": 77,
                "name": "Драматический сериал",
                "first_air_date": "2024-01-02"
            }]
        })))
        .mount(&server)
        .await;

    let page = TmdbClient::new(config(&server))
        .unwrap()
        .discover(TrendingMediaTypeDto::Tv, 18, 3)
        .await
        .unwrap();

    assert_eq!(page.genre_id, 18);
    assert_eq!(page.media_type, TrendingMediaTypeDto::Tv);
    assert_eq!(page.page, 3);
    assert_eq!(page.total_pages, 9);
    assert_eq!(page.results[0].tmdb_id, 77);
}

#[tokio::test]
async fn maps_last_partial_ui_page_without_skipping_provider_results() {
    let server = MockServer::start().await;
    let results = (21..=25)
        .map(|id| json!({"id": id, "title": format!("Movie {id}")}))
        .collect::<Vec<_>>();
    Mock::given(method("GET"))
        .and(path("/3/movie/upcoming"))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "page": 2,
            "total_pages": 2,
            "total_results": 25,
            "results": results
        })))
        .mount(&server)
        .await;

    let page = TmdbClient::new(config(&server))
        .unwrap()
        .premieres(TrendingMediaTypeDto::Movie, PremiereFeedDto::Upcoming, 3)
        .await
        .unwrap();

    assert_eq!(page.page, 3);
    assert_eq!(page.total_pages, 3);
    assert_eq!(page.results.len(), 5);
    assert_eq!(page.results[0].tmdb_id, 21);
    assert_eq!(page.results[4].tmdb_id, 25);
}

#[tokio::test]
async fn maps_every_supported_best_and_premiere_feed() {
    let server = MockServer::start().await;
    for endpoint in [
        "/3/tv/popular",
        "/3/movie/now_playing",
        "/3/movie/upcoming",
        "/3/tv/airing_today",
    ] {
        Mock::given(method("GET"))
            .and(path(endpoint))
            .and(query_param("page", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "page": 1,
                "total_pages": 1,
                "total_results": 0,
                "results": []
            })))
            .mount(&server)
            .await;
    }
    let client = TmdbClient::new(config(&server)).unwrap();

    client
        .best(TrendingMediaTypeDto::Tv, BestRankingDto::Popular, 1)
        .await
        .unwrap();
    for feed in [PremiereFeedDto::NowPlaying, PremiereFeedDto::Upcoming] {
        client
            .premieres(TrendingMediaTypeDto::Movie, feed, 1)
            .await
            .unwrap();
    }
    client
        .premieres(TrendingMediaTypeDto::Tv, PremiereFeedDto::AiringToday, 1)
        .await
        .unwrap();
}

#[tokio::test]
async fn rejects_mismatched_premiere_feed_and_invalid_discovery_page() {
    let server = MockServer::start().await;
    let client = TmdbClient::new(config(&server)).unwrap();

    assert_eq!(
        client
            .trending(TrendingCategoryDto::All, 0)
            .await
            .unwrap_err()
            .code(),
        TmdbErrorCode::InvalidRequest
    );
    assert_eq!(
        client
            .best(TrendingMediaTypeDto::Movie, BestRankingDto::Popular, 0)
            .await
            .unwrap_err()
            .code(),
        TmdbErrorCode::InvalidRequest
    );
    assert_eq!(
        client
            .premieres(TrendingMediaTypeDto::Tv, PremiereFeedDto::AiringToday, 0,)
            .await
            .unwrap_err()
            .code(),
        TmdbErrorCode::InvalidRequest
    );

    assert_eq!(
        client
            .premieres(TrendingMediaTypeDto::Movie, PremiereFeedDto::OnTheAir, 1,)
            .await
            .unwrap_err()
            .code(),
        TmdbErrorCode::InvalidRequest
    );
    assert_eq!(
        client
            .discover(TrendingMediaTypeDto::Tv, 0, 1)
            .await
            .unwrap_err()
            .code(),
        TmdbErrorCode::InvalidRequest
    );
    assert_eq!(
        client
            .discover(TrendingMediaTypeDto::Tv, 18, 0)
            .await
            .unwrap_err()
            .code(),
        TmdbErrorCode::InvalidRequest
    );
}
