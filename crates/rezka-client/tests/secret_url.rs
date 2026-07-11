use rezka_client::{PublicImageUrl, SecretMediaUrl, SecretSubtitleUrl};
use url::Url;

#[test]
fn accepts_signed_public_https_urls() {
    let media = SecretMediaUrl::new(
        Url::parse("https://cdn.example.invalid/video/master.m3u8?token=signed-token&expires=123")
            .unwrap(),
    )
    .unwrap();
    let subtitle = SecretSubtitleUrl::new(
        Url::parse("https://cdn.example.invalid/subtitles/en.vtt?signature=signed-token").unwrap(),
    )
    .unwrap();
    let image = PublicImageUrl::new(
        Url::parse("https://images.example.invalid/posters/1.jpg?width=640").unwrap(),
    )
    .unwrap();

    assert_eq!(
        media.with_url(|url| url.as_str().to_owned()),
        "https://cdn.example.invalid/video/master.m3u8?token=signed-token&expires=123"
    );
    assert_eq!(
        subtitle.with_url(|url| url.as_str().to_owned()),
        "https://cdn.example.invalid/subtitles/en.vtt?signature=signed-token"
    );
    assert_eq!(
        image.url().as_str(),
        "https://images.example.invalid/posters/1.jpg?width=640"
    );
}

#[test]
fn secret_media_url_formatting_is_exactly_redacted() {
    let url = SecretMediaUrl::new(
        Url::parse("https://cdn.example.invalid/video.mp4?token=signed-token").unwrap(),
    )
    .unwrap();

    assert_eq!(format!("{url:?}"), "SecretMediaUrl([REDACTED])");
    assert_eq!(format!("{url}"), "[REDACTED]");
}

#[test]
fn secret_subtitle_url_formatting_is_exactly_redacted() {
    let url = SecretSubtitleUrl::new(
        Url::parse("https://cdn.example.invalid/subtitles/en.vtt?token=signed-token").unwrap(),
    )
    .unwrap();

    assert_eq!(format!("{url:?}"), "SecretSubtitleUrl([REDACTED])");
    assert_eq!(format!("{url}"), "[REDACTED]");
}

#[test]
fn rejects_non_public_url_destinations_for_every_wrapper() {
    let invalid_urls = [
        "http://cdn.example.invalid/video.mp4",
        "https://user:password@cdn.example.invalid/video.mp4",
        "https://cdn.example.invalid/video.mp4#fragment",
        "https://localhost/video.mp4",
        "https://media.localhost/video.mp4",
        "https://127.0.0.1/video.mp4",
        "https://203.0.113.9/video.mp4",
        "https://[::1]/video.mp4",
        "https://[2001:db8::7]/video.mp4",
    ];

    for invalid in invalid_urls {
        let url = Url::parse(invalid).unwrap();
        assert!(
            SecretMediaUrl::new(url.clone()).is_err(),
            "accepted {invalid}"
        );
        assert!(
            SecretSubtitleUrl::new(url.clone()).is_err(),
            "accepted {invalid}"
        );
        assert!(PublicImageUrl::new(url).is_err(), "accepted {invalid}");
    }
}

#[test]
fn public_image_debug_is_redacted() {
    let image = PublicImageUrl::new(
        Url::parse("https://images.example.invalid/poster.jpg?token=signed-token").unwrap(),
    )
    .unwrap();
    let rendered = format!("{image:?}");

    assert_eq!(rendered, "PublicImageUrl([REDACTED])");
    assert!(!rendered.contains("images.example.invalid"));
    assert!(!rendered.contains("signed-token"));
}
