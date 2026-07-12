use std::fmt;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use scraper::Html;
use url::Url;

use crate::{RezkaError, SecretMediaUrl, playback::invalid_playback};

const MAX_STREAM_PAYLOAD_BYTES: usize = 1024 * 1024;
const MAX_STREAM_VARIANTS: usize = 32;
const MAX_ENDPOINTS_PER_VARIANT: usize = 4;
const MAX_SALT_MARKERS: usize = 60;
const SALT_MARKER: &str = "//_//";

#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd)]
pub enum QualityTier {
    Standard,
    Premium,
}

pub struct AdvertisedQuality {
    label: String,
    vertical_hint: Option<u16>,
    tier: QualityTier,
}

impl AdvertisedQuality {
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    #[must_use]
    pub const fn vertical_hint(&self) -> Option<u16> {
        self.vertical_hint
    }

    #[must_use]
    pub const fn tier(&self) -> QualityTier {
        self.tier
    }
}

impl fmt::Debug for AdvertisedQuality {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AdvertisedQuality")
            .field("label", &"[REDACTED]")
            .field("vertical_hint", &self.vertical_hint)
            .field("tier", &self.tier)
            .finish()
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum StreamKind {
    Hls,
    Mp4,
}

pub struct StreamEndpoint {
    kind: StreamKind,
    url: SecretMediaUrl,
}

impl StreamEndpoint {
    #[must_use]
    pub const fn kind(&self) -> StreamKind {
        self.kind
    }

    #[must_use]
    pub const fn url(&self) -> &SecretMediaUrl {
        &self.url
    }
}

impl fmt::Debug for StreamEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StreamEndpoint")
            .field("kind", &self.kind)
            .field("url", &self.url)
            .finish()
    }
}

pub struct StreamVariant {
    advertised_quality: AdvertisedQuality,
    endpoints: Vec<StreamEndpoint>,
}

impl StreamVariant {
    #[must_use]
    pub const fn advertised_quality(&self) -> &AdvertisedQuality {
        &self.advertised_quality
    }

    #[must_use]
    pub fn endpoints(&self) -> &[StreamEndpoint] {
        &self.endpoints
    }
}

impl fmt::Debug for StreamVariant {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StreamVariant")
            .field("advertised_quality", &self.advertised_quality)
            .field("endpoints", &self.endpoints)
            .finish()
    }
}

pub fn parse_stream_variants(payload: &str) -> Result<Vec<StreamVariant>, RezkaError> {
    let listing = decode_stream_payload(payload)?;
    if !listing.starts_with('[') {
        return Err(invalid_playback("stream listing missing quality"));
    }

    let entries = split_entries(&listing)?;
    let mut variants: Vec<StreamVariant> = Vec::new();
    for entry in entries {
        let close = entry
            .find(']')
            .ok_or_else(|| invalid_playback("stream quality is malformed"))?;
        let raw_label = entry
            .get(1..close)
            .ok_or_else(|| invalid_playback("stream quality is malformed"))?;
        let alternatives = entry
            .get(close + 1..)
            .ok_or_else(|| invalid_playback("stream alternatives missing"))?;
        let quality = normalize_quality(raw_label)?;
        let Some(endpoints) = parse_endpoints(alternatives)? else {
            continue;
        };

        if let Some(existing) = variants.iter_mut().find(|variant| {
            variant.advertised_quality.label == quality.label
                && variant.advertised_quality.tier == quality.tier
        }) {
            for endpoint in endpoints {
                if !contains_endpoint(&existing.endpoints, &endpoint) {
                    existing.endpoints.push(endpoint);
                }
                if existing.endpoints.len() > MAX_ENDPOINTS_PER_VARIANT {
                    return Err(invalid_playback("stream endpoint limit exceeded"));
                }
            }
        } else {
            variants.push(StreamVariant {
                advertised_quality: quality,
                endpoints,
            });
            if variants.len() > MAX_STREAM_VARIANTS {
                return Err(invalid_playback("stream variant limit exceeded"));
            }
        }
    }

    if variants.is_empty() {
        return Err(invalid_playback("stream listing is empty"));
    }
    variants.sort_by(|left, right| {
        right
            .advertised_quality
            .vertical_hint
            .cmp(&left.advertised_quality.vertical_hint)
            .then_with(|| {
                right
                    .advertised_quality
                    .tier
                    .cmp(&left.advertised_quality.tier)
            })
            .then_with(|| {
                left.advertised_quality
                    .label
                    .cmp(&right.advertised_quality.label)
            })
    });
    Ok(variants)
}

fn decode_stream_payload(payload: &str) -> Result<String, RezkaError> {
    if payload.len() > MAX_STREAM_PAYLOAD_BYTES {
        return Err(invalid_playback("stream payload exceeds limit"));
    }
    if !payload.starts_with("#h") {
        return Ok(payload.to_owned());
    }

    let encoded = payload
        .strip_prefix("#h")
        .ok_or_else(|| invalid_playback("stream encoding is invalid"))?;
    let marker_count = encoded.matches(SALT_MARKER).count();
    if marker_count > MAX_SALT_MARKERS {
        return Err(invalid_playback("stream salt marker limit exceeded"));
    }

    let mut parts = encoded.split(SALT_MARKER);
    let mut cleaned = parts.next().unwrap_or_default().to_owned();
    for part in parts {
        let salt_len = known_salt_prefix_len(part).or_else(|| (part.len() >= 16).then_some(16));
        let salt_len = salt_len.ok_or_else(|| invalid_playback("stream salt is malformed"))?;
        let payload = part
            .get(salt_len..)
            .ok_or_else(|| invalid_playback("stream salt is malformed"))?;
        cleaned.push_str(payload);
        if cleaned.len() > MAX_STREAM_PAYLOAD_BYTES {
            return Err(invalid_playback("stream payload exceeds limit"));
        }
    }

    let decoded = STANDARD
        .decode(cleaned.as_bytes())
        .map_err(|_| invalid_playback("stream encoding is invalid"))?;
    if decoded.len() > MAX_STREAM_PAYLOAD_BYTES {
        return Err(invalid_playback("stream payload exceeds limit"));
    }
    String::from_utf8(decoded).map_err(|_| invalid_playback("stream encoding is invalid"))
}

fn known_salt_prefix_len(value: &str) -> Option<usize> {
    let encoded = value.get(..4)?;
    let decoded = STANDARD.decode(encoded.as_bytes()).ok()?;
    ((2..=3).contains(&decoded.len())
        && decoded
            .iter()
            .all(|byte| matches!(byte, b'@' | b'#' | b'!' | b'^' | b'$')))
    .then_some(4)
}

fn split_entries(listing: &str) -> Result<Vec<&str>, RezkaError> {
    let bytes = listing.as_bytes();
    let mut entries = Vec::new();
    let mut start = 0;
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b',' {
            if bytes.get(index + 1) != Some(&b'[') {
                return Err(invalid_playback("stream listing has trailing data"));
            }
            entries.push(&listing[start..index]);
            start = index + 1;
        }
    }
    entries.push(&listing[start..]);
    if entries.iter().any(|entry| entry.is_empty()) {
        return Err(invalid_playback("stream listing entry is empty"));
    }
    Ok(entries)
}

fn normalize_quality(raw: &str) -> Result<AdvertisedQuality, RezkaError> {
    let document = Html::parse_fragment(raw);
    let label = document
        .root_element()
        .text()
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if label.is_empty() || label.len() > 4_096 {
        return Err(invalid_playback("stream quality is invalid"));
    }
    let lowered = format!("{raw} {label}").to_ascii_lowercase();
    let tier = if lowered.contains("premium") || lowered.contains("ultra") {
        QualityTier::Premium
    } else {
        QualityTier::Standard
    };
    let vertical_hint = label
        .split(|character: char| !character.is_ascii_digit())
        .find(|part| !part.is_empty())
        .and_then(|part| part.parse::<u16>().ok())
        .filter(|value| *value > 0);
    Ok(AdvertisedQuality {
        label,
        vertical_hint,
        tier,
    })
}

// Returns `Ok(None)` when a variant carries a non-public or insecure stream URL: such a variant is
// skipped so a single degraded alternative does not fail the whole manifest. Structural malformation
// (bad separators, unknown endpoint kinds, exceeded budgets) still fails with `Err`.
fn parse_endpoints(alternatives: &str) -> Result<Option<Vec<StreamEndpoint>>, RezkaError> {
    let raw = alternatives
        .split(" or ")
        .map(str::trim)
        .collect::<Vec<_>>();
    if raw.is_empty() || raw.iter().any(|value| value.is_empty()) {
        return Err(invalid_playback("stream alternatives are invalid"));
    }
    let modern = raw
        .iter()
        .any(|value| value.ends_with(":hls:manifest.m3u8"));
    let mut endpoints = Vec::new();
    for (index, value) in raw.iter().enumerate() {
        let url = Url::parse(value).map_err(|_| invalid_playback("stream URL is invalid"))?;
        let kind = if modern {
            if value.ends_with(":hls:manifest.m3u8") {
                StreamKind::Hls
            } else if url.path().ends_with(".mp4") {
                StreamKind::Mp4
            } else {
                return Err(invalid_playback("stream endpoint kind is invalid"));
            }
        } else if raw.len() == 2 {
            if index == 0 {
                StreamKind::Hls
            } else {
                StreamKind::Mp4
            }
        } else if url.path().ends_with(".m3u8") {
            StreamKind::Hls
        } else if url.path().ends_with(".mp4") {
            StreamKind::Mp4
        } else {
            return Err(invalid_playback("stream endpoint kind is invalid"));
        };
        let url = match SecretMediaUrl::new(url) {
            Ok(url) => url,
            Err(_) => return Ok(None),
        };
        let endpoint = StreamEndpoint { kind, url };
        if !contains_endpoint(&endpoints, &endpoint) {
            endpoints.push(endpoint);
        }
        if endpoints.len() > MAX_ENDPOINTS_PER_VARIANT {
            return Err(invalid_playback("stream endpoint limit exceeded"));
        }
    }
    endpoints.sort_by_key(|endpoint| match endpoint.kind {
        StreamKind::Hls => 0,
        StreamKind::Mp4 => 1,
    });
    Ok(Some(endpoints))
}

fn contains_endpoint(endpoints: &[StreamEndpoint], candidate: &StreamEndpoint) -> bool {
    endpoints.iter().any(|endpoint| {
        endpoint.kind == candidate.kind
            && endpoint.url.with_url(|left| {
                candidate
                    .url
                    .with_url(|right| left.as_str() == right.as_str())
            })
    })
}
