mod support;

use std::sync::{Arc, Mutex};

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use media_api::{MediaDetailsService, MediaDetailsServiceError, router};
use media_contract::{MediaDetailsDto, SimilarPageDto, TrendingItemDto, TrendingMediaTypeDto};
use media_core::{PRIMARY_CLIENT_ID, PRIMARY_USER_ID, Actor, ClientRole};
use tower::ServiceExt;

use support::{FakeClientStore, FakeReadiness, VALID_TOKEN, state};

struct FakeMediaDetailsService {
    details_calls: Mutex<Vec<(u64, TrendingMediaTypeDto)>>,
    similar_calls: Mutex<Vec<(u64, TrendingMediaTypeDto, u32)>>,
}

#[async_trait::async_trait]
impl MediaDetailsService for FakeMediaDetailsService {
    async fn details(
        &self,
        tmdb_id: u64,
        media_type: TrendingMediaTypeDto,
    ) -> Result<MediaDetailsDto, MediaDetailsServiceError> {
        self.details_calls
            .lock()
            .unwrap()
            .push((tmdb_id, media_type));
        Ok(MediaDetailsDto {
            tmdb_id,
            media_type,
            title: "Тестовый фильм".to_owned(),
            original_title: Some("Test Movie".to_owned()),
            release_date: Some("2026-01-02".to_owned()),
            year: Some(2026),
            rating: Some(8.1),
            poster_url: Some("https://image.tmdb.org/t/p/w780/poster.jpg".to_owned()),
            overview: Some("Описание".to_owned()),
            countries: vec!["Canada".to_owned()],
            genres: vec!["Drama".to_owned()],
            status: Some("Released".to_owned()),
            season_count: None,
            episode_count: None,
            tmdb_url: Some("https://www.themoviedb.org/movie/7".to_owned()),
            imdb_url: Some("https://www.imdb.com/title/tt1234567/".to_owned()),
            trailer_url: None,
        })
    }

    async fn similar(
        &self,
        tmdb_id: u64,
        media_type: TrendingMediaTypeDto,
        page: u32,
    ) -> Result<SimilarPageDto, MediaDetailsServiceError> {
        self.similar_calls
            .lock()
            .unwrap()
            .push((tmdb_id, media_type, page));
        Ok(SimilarPageDto {
            source: "tmdb".to_owned(),
            tmdb_id,
            media_type,
            page,
            total_pages: 2,
            total_results: 11,
            results: vec![TrendingItemDto {
                tmdb_id: 8,
                media_type,
                title: "Похожий фильм".to_owned(),
                original_title: Some("Similar Movie".to_owned()),
                year: Some(2025),
                rating: Some(7.7),
                poster_url: None,
                overview: None,
            }],
        })
    }
}

fn actor() -> Actor {
    Actor::new(PRIMARY_CLIENT_ID, Some(PRIMARY_USER_ID), ClientRole::Hermes).unwrap()
}

fn auth_request(uri: &str) -> Request<Body> {
    Request::get(uri)
        .header(header::AUTHORIZATION, format!("Bearer {VALID_TOKEN}"))
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
async fn details_and_similar_are_authenticated_and_forwarded() {
    let service = Arc::new(FakeMediaDetailsService {
        details_calls: Mutex::new(Vec::new()),
        similar_calls: Mutex::new(Vec::new()),
    });
    let app = router(
        state(
            FakeClientStore::new([(VALID_TOKEN, actor())]),
            FakeReadiness::ready(),
        )
        .with_media_details(service.clone()),
    );

    assert_eq!(
        app.clone()
            .oneshot(auth_request("/v1/media/details?tmdb_id=7&media_type=movie"))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let response = app
        .clone()
        .oneshot(auth_request(
            "/v1/media/similar?tmdb_id=7&media_type=movie&page=2",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(body["results"][0]["title"], "Похожий фильм");
    assert_eq!(
        *service.details_calls.lock().unwrap(),
        vec![(7, TrendingMediaTypeDto::Movie)]
    );
    assert_eq!(
        *service.similar_calls.lock().unwrap(),
        vec![(7, TrendingMediaTypeDto::Movie, 2)]
    );
}

#[tokio::test]
async fn details_routes_validate_ids_and_pages() {
    let app = router(
        state(
            FakeClientStore::new([(VALID_TOKEN, actor())]),
            FakeReadiness::ready(),
        )
        .with_media_details(Arc::new(FakeMediaDetailsService {
            details_calls: Mutex::new(Vec::new()),
            similar_calls: Mutex::new(Vec::new()),
        })),
    );

    for uri in [
        "/v1/media/details?tmdb_id=0&media_type=movie",
        "/v1/media/similar?tmdb_id=7&media_type=movie&page=0",
    ] {
        let response = app.clone().oneshot(auth_request(uri)).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
