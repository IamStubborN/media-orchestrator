mod support;

use std::sync::{Arc, Mutex};

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use media_api::{TrendingService, TrendingServiceError, router};
use media_contract::{TrendingCategoryDto, TrendingItemDto, TrendingMediaTypeDto, TrendingPageDto};
use media_core::{Actor, ClientRole, PRIMARY_CLIENT_ID, PRIMARY_USER_ID};
use tower::ServiceExt;

use support::{FakeClientStore, FakeReadiness, VALID_TOKEN, state};

struct FakeTrendingService {
    request: Mutex<Option<(TrendingCategoryDto, u32)>>,
}

#[async_trait::async_trait]
impl TrendingService for FakeTrendingService {
    async fn trending(
        &self,
        category: TrendingCategoryDto,
        page: u32,
    ) -> Result<TrendingPageDto, TrendingServiceError> {
        *self.request.lock().unwrap() = Some((category, page));
        Ok(TrendingPageDto {
            source: "tmdb".to_owned(),
            window: "week".to_owned(),
            category,
            page,
            total_pages: 3,
            total_results: 42,
            results: vec![TrendingItemDto {
                tmdb_id: 7,
                media_type: TrendingMediaTypeDto::Movie,
                title: "Тестовый фильм".to_owned(),
                original_title: Some("Test Movie".to_owned()),
                year: Some(2026),
                rating: Some(8.1),
                poster_url: Some("https://image.tmdb.org/t/p/w780/test.jpg".to_owned()),
                overview: Some("Test overview".to_owned()),
            }],
        })
    }
}

fn actor() -> Actor {
    Actor::new(PRIMARY_CLIENT_ID, Some(PRIMARY_USER_ID), ClientRole::Hermes).unwrap()
}

#[tokio::test]
async fn trending_is_authenticated_and_forwards_category_and_page() {
    let service = Arc::new(FakeTrendingService {
        request: Mutex::new(None),
    });
    let app = router(
        state(
            FakeClientStore::new([(VALID_TOKEN, actor())]),
            FakeReadiness::ready(),
        )
        .with_trending(service.clone()),
    );

    let unauthorized = app
        .clone()
        .oneshot(
            Request::get("/v1/trending?category=movie&page=2")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let response = app
        .oneshot(
            Request::get("/v1/trending?category=movie&page=2")
                .header(header::AUTHORIZATION, format!("Bearer {VALID_TOKEN}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(body["source"], "tmdb");
    assert_eq!(body["results"][0]["title"], "Тестовый фильм");
    assert_eq!(
        *service.request.lock().unwrap(),
        Some((TrendingCategoryDto::Movie, 2))
    );
}

#[tokio::test]
async fn trending_defaults_to_all_page_one_and_validates_query() {
    let service = Arc::new(FakeTrendingService {
        request: Mutex::new(None),
    });
    let app = router(
        state(
            FakeClientStore::new([(VALID_TOKEN, actor())]),
            FakeReadiness::ready(),
        )
        .with_trending(service.clone()),
    );
    let authorized = || {
        Request::get("/v1/trending")
            .header(header::AUTHORIZATION, format!("Bearer {VALID_TOKEN}"))
            .body(Body::empty())
            .unwrap()
    };
    assert_eq!(
        app.clone().oneshot(authorized()).await.unwrap().status(),
        StatusCode::OK
    );
    assert_eq!(
        *service.request.lock().unwrap(),
        Some((TrendingCategoryDto::All, 1))
    );

    for uri in ["/v1/trending?category=person", "/v1/trending?page=0"] {
        let response = app
            .clone()
            .oneshot(
                Request::get(uri)
                    .header(header::AUTHORIZATION, format!("Bearer {VALID_TOKEN}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn missing_tmdb_configuration_is_service_unavailable() {
    let app = router(state(
        FakeClientStore::new([(VALID_TOKEN, actor())]),
        FakeReadiness::ready(),
    ));
    let response = app
        .oneshot(
            Request::get("/v1/trending")
                .header(header::AUTHORIZATION, format!("Bearer {VALID_TOKEN}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}
