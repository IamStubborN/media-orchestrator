use std::{collections::BTreeMap, fmt};

use scraper::{Html, Selector};
use serde::de::{self, MapAccess, Visitor};

use super::{
    EpisodeAvailability, PlaybackManifest, PlaybackRequest, ResolvedTarget, SeasonAvailability,
    SelectedTranslation, SeriesAvailability, invalid_playback,
};
use crate::{
    ProviderFailureReason, RezkaError, parse_stream_variants, parse_subtitle_fields,
    redaction::sanitize_provider_text, session::anubis::detect_challenge,
};

const MAX_SEASONS: usize = 256;
const MAX_EPISODES_PER_SEASON: usize = 4_096;
const MAX_TOTAL_EPISODES: usize = 16_384;
const MAX_LABEL_BYTES: usize = 4_096;

pub fn parse_playback_manifest(
    wrapper_json: &str,
    request: PlaybackRequest,
) -> Result<PlaybackManifest, RezkaError> {
    if detect_challenge(wrapper_json) {
        return Err(RezkaError::ChallengeRequired {
            context: sanitize_provider_text("playback challenge detected"),
        });
    }
    let wrapper: PlaybackWrapper = serde_json::from_str(wrapper_json)
        .map_err(|_| invalid_playback("invalid playback response"))?;
    if !wrapper.success {
        return Err(playback_failure(wrapper.message.as_deref()));
    }
    let stream_payload = wrapper
        .url
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| invalid_playback("playback streams missing"))?;
    let variants = parse_stream_variants(&stream_payload)?;
    let subtitles = parse_subtitle_fields(wrapper_json)?;
    let (title, translation, target) = match request {
        PlaybackRequest::Movie(selection) => (
            selection.title,
            *selection.translation.key(),
            ResolvedTarget::Movie,
        ),
        PlaybackRequest::Episode(selected) => (
            selected.selection.title,
            *selected.selection.translation.key(),
            ResolvedTarget::Episode {
                season: selected.season,
                episode: selected.episode,
            },
        ),
    };
    Ok(PlaybackManifest::new(
        title,
        translation,
        target,
        variants,
        subtitles,
    ))
}

fn playback_failure(message: Option<&str>) -> RezkaError {
    let normalized = message.unwrap_or_default().trim().to_ascii_lowercase();
    match normalized.as_str() {
        "translation unavailable" => RezkaError::TranslationUnavailable {
            reason: ProviderFailureReason::TranslationUnavailable,
        },
        "episode unavailable" => RezkaError::EpisodeUnavailable {
            reason: ProviderFailureReason::EpisodeUnavailable,
        },
        "premium required" => RezkaError::QualityUnavailable {
            reason: ProviderFailureReason::PremiumRequired,
        },
        "authentication required" => RezkaError::AuthenticationRequired {
            context: sanitize_provider_text("playback authentication required"),
        },
        _ => RezkaError::QualityUnavailable {
            reason: ProviderFailureReason::Unknown,
        },
    }
}

struct PlaybackWrapper {
    success: bool,
    url: Option<String>,
    message: Option<String>,
}

impl<'de> serde::Deserialize<'de> for PlaybackWrapper {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_map(PlaybackWrapperVisitor)
    }
}

struct PlaybackWrapperVisitor;

impl<'de> Visitor<'de> for PlaybackWrapperVisitor {
    type Value = PlaybackWrapper;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a playback response object")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut success = None;
        let mut url = None;
        let mut message = None;
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "success" => set_once(&mut success, map.next_value()?, "success")?,
                "url" => set_once(&mut url, map.next_value()?, "url")?,
                "message" => set_once(&mut message, map.next_value()?, "message")?,
                _ => {
                    let _: de::IgnoredAny = map.next_value()?;
                }
            }
        }
        Ok(PlaybackWrapper {
            success: success.ok_or_else(|| de::Error::missing_field("success"))?,
            url,
            message,
        })
    }
}

pub fn parse_series_availability(
    wrapper_json: &str,
    selection: SelectedTranslation,
) -> Result<SeriesAvailability, RezkaError> {
    let wrapper: EpisodesWrapper = serde_json::from_str(wrapper_json)
        .map_err(|_| invalid_playback("invalid availability response"))?;
    if !wrapper.success {
        return Err(invalid_playback("availability request failed"));
    }

    let seasons_html = wrapper
        .seasons
        .ok_or_else(|| invalid_playback("availability seasons missing"))?;
    let episodes_html = wrapper
        .episodes
        .ok_or_else(|| invalid_playback("availability episodes missing"))?;
    let mut seasons = parse_seasons(&seasons_html)?;
    let episodes = parse_episodes(&episodes_html)?;
    let mut total_episodes = 0_usize;

    for ((season_number, episode_number), label) in episodes {
        let season = seasons
            .get_mut(&season_number)
            .ok_or_else(|| invalid_playback("episode references unknown season"))?;
        season
            .1
            .push(EpisodeAvailability::new(episode_number, label));
        total_episodes += 1;
        if season.1.len() > MAX_EPISODES_PER_SEASON || total_episodes > MAX_TOTAL_EPISODES {
            return Err(invalid_playback("episode limit exceeded"));
        }
    }

    let seasons = seasons
        .into_iter()
        .map(|(number, (label, mut episodes))| {
            episodes.sort_by_key(EpisodeAvailability::number);
            SeasonAvailability::new(number, label, episodes)
        })
        .collect();
    Ok(SeriesAvailability::new(selection, seasons))
}

fn parse_seasons(
    fragment: &str,
) -> Result<BTreeMap<u32, (String, Vec<EpisodeAvailability>)>, RezkaError> {
    let document = Html::parse_fragment(fragment);
    let selector = Selector::parse("[data-tab_id]").expect("static selector is valid");
    let mut seasons = BTreeMap::new();
    for element in document.select(&selector) {
        let number = parse_number(element.value().attr("data-tab_id"))?;
        let label = normalized_label(element.text())?;
        if seasons.insert(number, (label, Vec::new())).is_some() {
            return Err(invalid_playback("duplicate season"));
        }
        if seasons.len() > MAX_SEASONS {
            return Err(invalid_playback("season limit exceeded"));
        }
    }
    Ok(seasons)
}

fn parse_episodes(fragment: &str) -> Result<BTreeMap<(u32, u32), String>, RezkaError> {
    let document = Html::parse_fragment(fragment);
    let selector =
        Selector::parse("[data-season_id][data-episode_id]").expect("static selector is valid");
    let mut episodes = BTreeMap::new();
    for element in document.select(&selector) {
        let season = parse_number(element.value().attr("data-season_id"))?;
        let episode = parse_number(element.value().attr("data-episode_id"))?;
        let label = normalized_label(element.text())?;
        if episodes.insert((season, episode), label).is_some() {
            return Err(invalid_playback("duplicate episode"));
        }
        if episodes.len() > MAX_TOTAL_EPISODES {
            return Err(invalid_playback("episode limit exceeded"));
        }
    }
    Ok(episodes)
}

fn parse_number(value: Option<&str>) -> Result<u32, RezkaError> {
    let value = value
        .ok_or_else(|| invalid_playback("availability number missing"))?
        .parse::<u32>()
        .map_err(|_| invalid_playback("invalid availability number"))?;
    if value == 0 || value > i32::MAX as u32 {
        return Err(invalid_playback("invalid availability number"));
    }
    Ok(value)
}

fn normalized_label<'a>(text: impl Iterator<Item = &'a str>) -> Result<String, RezkaError> {
    let label = text.collect::<Vec<_>>().join(" ");
    let label = label.split_whitespace().collect::<Vec<_>>().join(" ");
    if label.is_empty() || label.len() > MAX_LABEL_BYTES {
        return Err(invalid_playback("invalid availability label"));
    }
    Ok(label)
}

struct EpisodesWrapper {
    success: bool,
    seasons: Option<String>,
    episodes: Option<String>,
}

impl<'de> serde::Deserialize<'de> for EpisodesWrapper {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_map(EpisodesWrapperVisitor)
    }
}

struct EpisodesWrapperVisitor;

impl<'de> Visitor<'de> for EpisodesWrapperVisitor {
    type Value = EpisodesWrapper;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an availability response object")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut success = None;
        let mut seasons = None;
        let mut episodes = None;
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "success" => set_once(&mut success, map.next_value()?, "success")?,
                "seasons" => set_once(&mut seasons, map.next_value()?, "seasons")?,
                "episodes" => set_once(&mut episodes, map.next_value()?, "episodes")?,
                _ => {
                    let _: de::IgnoredAny = map.next_value()?;
                }
            }
        }
        Ok(EpisodesWrapper {
            success: success.ok_or_else(|| de::Error::missing_field("success"))?,
            seasons,
            episodes,
        })
    }
}

fn set_once<T, E: de::Error>(slot: &mut Option<T>, value: T, field: &'static str) -> Result<(), E> {
    if slot.replace(value).is_some() {
        return Err(E::duplicate_field(field));
    }
    Ok(())
}
