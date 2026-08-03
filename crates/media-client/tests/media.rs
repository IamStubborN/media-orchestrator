use media_client::MediaClient;
use media_contract::TrendingMediaTypeDto;
use secrecy::SecretString;
use serde_json::json;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path, query_param},
};

#[tokio::test]
async fn requests_details_and_similar_through_the_http_contract() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/media/details"))
        .and(query_param("tmdb_id", "7"))
        .and(query_param("media_type", "movie"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "tmdb_id": 7,
            "media_type": "movie",
            "title": "Тестовый фильм"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/media/similar"))
        .and(query_param("tmdb_id", "7"))
        .and(query_param("media_type", "movie"))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "source": "tmdb",
            "tmdb_id": 7,
            "media_type": "movie",
            "page": 2,
            "total_pages": 1,
            "total_results": 0,
            "results": []
        })))
        .mount(&server)
        .await;

    let base_url = format!("{}/", server.uri()).parse().unwrap();
    let client = MediaClient::new((base_url, SecretString::from("test-token"))).unwrap();

    let details = client
        .media_details(7, TrendingMediaTypeDto::Movie)
        .await
        .unwrap();
    assert_eq!(details.tmdb_id, 7);
    assert_eq!(details.title, "Тестовый фильм");

    let similar = client
        .media_similar(7, TrendingMediaTypeDto::Movie, 2)
        .await
        .unwrap();
    assert_eq!(similar.tmdb_id, 7);
    assert_eq!(similar.page, 2);
    assert!(similar.results.is_empty());
}
