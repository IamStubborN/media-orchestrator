use std::time::{Duration, Instant};

use crate::{composition, config::RunnerConfig};

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum InspectionKind {
    Movie {
        director: bool,
        camrip: bool,
        has_ads: bool,
    },
    Episode {
        season: u32,
        episode: u32,
    },
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PlaybackInspection {
    pub title_id: u64,
    pub target: String,
    pub variants: Vec<VariantInspection>,
    pub subtitles: Vec<SubtitleInspection>,
    pub stream_probes: Vec<StreamProbeInspection>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct StreamProbeInspection {
    pub host: String,
    pub kind: String,
    pub request: String,
    pub status: Option<u16>,
    pub content_type: Option<String>,
    pub elapsed_ms: u128,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct VariantInspection {
    pub advertised_quality: String,
    pub advertised_height: Option<u16>,
    pub premium: bool,
    pub stream_kinds: Vec<String>,
    pub stream_hosts: Vec<String>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SubtitleInspection {
    pub label: String,
    pub language: Option<String>,
    pub alternatives: usize,
    pub hosts: Vec<String>,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, thiserror::Error)]
pub enum DiagnosticError {
    #[error("Rezka diagnostic session construction failed")]
    Session,
    #[error("Rezka encrypted session snapshot could not be restored")]
    SessionStore,
    #[error("Rezka session validation failed")]
    Authentication,
    #[error("Rezka title resolution failed")]
    Title,
    #[error("Rezka title identity did not match")]
    Identity,
    #[error("Rezka translation selection failed")]
    Translation,
    #[error("Rezka episode selection failed")]
    Episode,
    #[error("Rezka playback resolution failed")]
    Playback,
    #[error("Rezka diagnostic session persistence failed")]
    Persistence,
}

impl PlaybackInspection {
    #[must_use]
    pub fn as_json(&self) -> serde_json::Value {
        serde_json::json!({
            "title_id": self.title_id,
            "target": self.target,
            "variants": self.variants.iter().map(|variant| serde_json::json!({
                "advertised_quality": variant.advertised_quality,
                "advertised_height": variant.advertised_height,
                "premium": variant.premium,
                "stream_kinds": variant.stream_kinds,
                "stream_hosts": variant.stream_hosts,
            })).collect::<Vec<_>>(),
            "subtitles": self.subtitles.iter().map(|subtitle| serde_json::json!({
                "label": subtitle.label,
                "language": subtitle.language,
                "alternatives": subtitle.alternatives,
                "hosts": subtitle.hosts,
            })).collect::<Vec<_>>(),
            "stream_probes": self.stream_probes.iter().map(|probe| serde_json::json!({
                "host": probe.host,
                "kind": probe.kind,
                "request": probe.request,
                "status": probe.status,
                "content_type": probe.content_type,
                "elapsed_ms": probe.elapsed_ms,
                "error": probe.error,
            })).collect::<Vec<_>>(),
        })
    }
}

pub async fn inspect_playback(
    config: &RunnerConfig,
    locator: &str,
    expected_title_id: u64,
    translation_id: u64,
    kind: InspectionKind,
    probe_streams: bool,
) -> Result<PlaybackInspection, DiagnosticError> {
    let mut prepared =
        composition::prepare_runner_session(config).map_err(|error| match error {
            composition::RunnerCompositionError::SessionStore => DiagnosticError::SessionStore,
            composition::RunnerCompositionError::Client
            | composition::RunnerCompositionError::Probe
            | composition::RunnerCompositionError::Store => DiagnosticError::Session,
        })?;
    let session_guard = prepared
        .acquire_session_lock()
        .await
        .map_err(|_| DiagnosticError::Session)?;
    prepared.reload_session().map_err(|error| match error {
        composition::RunnerCompositionError::SessionStore => DiagnosticError::SessionStore,
        composition::RunnerCompositionError::Client
        | composition::RunnerCompositionError::Probe
        | composition::RunnerCompositionError::Store => DiagnosticError::Session,
    })?;
    prepared
        .client
        .validate_session(&prepared.probe)
        .await
        .map_err(|_| DiagnosticError::Authentication)?;
    let locator = rezka_client::TitleLocator::new(locator).map_err(|_| DiagnosticError::Title)?;
    let details = prepared
        .client
        .title(&locator)
        .await
        .map_err(|_| DiagnosticError::Title)?;
    if details.id().get() != expected_title_id {
        return Err(DiagnosticError::Identity);
    }
    let translation_id = rezka_client::TranslationId::new(translation_id)
        .map_err(|_| DiagnosticError::Translation)?;
    let key = match kind {
        InspectionKind::Movie {
            director,
            camrip,
            has_ads,
        } => rezka_client::TranslationKey::Movie {
            id: translation_id,
            is_camrip: camrip,
            has_ads,
            is_director: director,
        },
        InspectionKind::Episode { .. } => {
            rezka_client::TranslationKey::Series { id: translation_id }
        }
    };
    let selection = details
        .select_translation(&key)
        .map_err(|_| DiagnosticError::Translation)?;
    let request = match kind {
        InspectionKind::Movie { .. } => selection
            .movie_request()
            .map_err(|_| DiagnosticError::Translation)?,
        InspectionKind::Episode { season, episode } => prepared
            .client
            .series_availability(&selection)
            .await
            .map_err(|_| DiagnosticError::Episode)?
            .select_episode(season, episode)
            .map_err(|_| DiagnosticError::Episode)?
            .playback_request(),
    };
    let manifest = prepared
        .client
        .resolve(request)
        .await
        .map_err(|_| DiagnosticError::Playback)?;
    let snapshot = prepared
        .client
        .export_session()
        .map_err(|_| DiagnosticError::Persistence)?;
    prepared
        .store
        .save(&snapshot)
        .map_err(|_| DiagnosticError::Persistence)?;
    drop(session_guard);

    let mut stream_probes = Vec::new();
    if probe_streams {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .map_err(|_| DiagnosticError::Session)?;
        if let Some(variant) = highest_standard_variant(manifest.variants()) {
            for endpoint in variant.endpoints() {
                match endpoint.kind() {
                    rezka_client::StreamKind::Hls => {
                        stream_probes.push(probe_stream(&client, endpoint, true).await);
                        stream_probes.push(probe_stream(&client, endpoint, false).await);
                    }
                    rezka_client::StreamKind::Mp4 => {
                        stream_probes.push(probe_stream(&client, endpoint, true).await);
                    }
                }
            }
        }
    }

    Ok(PlaybackInspection {
        title_id: manifest.title().id().get(),
        target: manifest.target().season_episode().map_or_else(
            || "movie".to_owned(),
            |(season, episode)| format!("S{season:02}E{episode:02}"),
        ),
        variants: manifest
            .variants()
            .iter()
            .map(|variant| VariantInspection {
                advertised_quality: variant.advertised_quality().label().to_owned(),
                advertised_height: variant.advertised_quality().vertical_hint(),
                premium: variant.advertised_quality().tier() == rezka_client::QualityTier::Premium,
                stream_kinds: variant
                    .endpoints()
                    .iter()
                    .map(|endpoint| match endpoint.kind() {
                        rezka_client::StreamKind::Hls => "hls".to_owned(),
                        rezka_client::StreamKind::Mp4 => "mp4".to_owned(),
                    })
                    .collect(),
                stream_hosts: variant
                    .endpoints()
                    .iter()
                    .filter_map(|endpoint| {
                        endpoint
                            .url()
                            .with_url(|url| url.host_str().map(str::to_owned))
                    })
                    .collect(),
            })
            .collect(),
        subtitles: manifest
            .subtitles()
            .iter()
            .map(|track| SubtitleInspection {
                label: track.label().to_owned(),
                language: track
                    .language()
                    .map(|language| language.as_str().to_owned()),
                alternatives: track.alternatives().len(),
                hosts: track
                    .alternatives()
                    .iter()
                    .filter_map(|url| url.with_url(|url| url.host_str().map(str::to_owned)))
                    .collect(),
            })
            .collect(),
        stream_probes,
    })
}

fn highest_standard_variant(
    variants: &[rezka_client::StreamVariant],
) -> Option<&rezka_client::StreamVariant> {
    variants
        .iter()
        .filter(|variant| {
            variant.advertised_quality().tier() == rezka_client::QualityTier::Standard
        })
        .max_by_key(|variant| {
            variant
                .advertised_quality()
                .vertical_hint()
                .unwrap_or_default()
        })
}

async fn probe_stream(
    client: &reqwest::Client,
    endpoint: &rezka_client::StreamEndpoint,
    range: bool,
) -> StreamProbeInspection {
    let (url, host) = endpoint
        .url()
        .with_url(|url| (url.clone(), url.host_str().unwrap_or("unknown").to_owned()));
    let kind = match endpoint.kind() {
        rezka_client::StreamKind::Hls => "hls",
        rezka_client::StreamKind::Mp4 => "mp4",
    };
    let request = if range { "range" } else { "get" };
    let mut builder = client.get(url);
    if range {
        builder = builder.header(reqwest::header::RANGE, "bytes=0-0");
    }
    let started = Instant::now();
    let result = tokio::time::timeout(Duration::from_secs(15), builder.send()).await;
    let elapsed_ms = started.elapsed().as_millis();
    match result {
        Ok(Ok(response)) => StreamProbeInspection {
            host,
            kind: kind.to_owned(),
            request: request.to_owned(),
            status: Some(response.status().as_u16()),
            content_type: response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned),
            elapsed_ms,
            error: None,
        },
        Ok(Err(error)) => StreamProbeInspection {
            host,
            kind: kind.to_owned(),
            request: request.to_owned(),
            status: error.status().map(|status| status.as_u16()),
            content_type: None,
            elapsed_ms,
            error: Some(
                if error.is_timeout() {
                    "timeout"
                } else if error.is_connect() {
                    "connect"
                } else if error.is_request() {
                    "request"
                } else if error.is_body() {
                    "body"
                } else {
                    "transport"
                }
                .to_owned(),
            ),
        },
        Err(_) => StreamProbeInspection {
            host,
            kind: kind.to_owned(),
            request: request.to_owned(),
            status: None,
            content_type: None,
            elapsed_ms,
            error: Some("deadline".to_owned()),
        },
    }
}
