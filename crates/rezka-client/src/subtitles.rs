use std::{collections::BTreeMap, fmt};

use serde::de::{self, MapAccess, Visitor};
use url::Url;

use crate::{RezkaError, SecretSubtitleUrl, playback::invalid_playback};

const MAX_SUBTITLE_TRACKS: usize = 64;
const MAX_ALTERNATIVES_PER_TRACK: usize = 4;

pub struct SubtitleTrackId {
    provider_label: String,
    ordinal: u16,
}

impl SubtitleTrackId {
    #[must_use]
    pub fn provider_label(&self) -> &str {
        &self.provider_label
    }

    #[must_use]
    pub const fn ordinal(&self) -> u16 {
        self.ordinal
    }
}

impl fmt::Debug for SubtitleTrackId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SubtitleTrackId")
            .field("provider_label", &"[REDACTED]")
            .field("ordinal", &self.ordinal)
            .finish()
    }
}

pub struct SubtitleLanguage(String);

impl SubtitleLanguage {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SubtitleLanguage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SubtitleLanguage([REDACTED])")
    }
}

pub struct SubtitleTrack {
    id: SubtitleTrackId,
    language: Option<SubtitleLanguage>,
    label: String,
    alternatives: Vec<SecretSubtitleUrl>,
}

impl SubtitleTrack {
    #[must_use]
    pub const fn id(&self) -> &SubtitleTrackId {
        &self.id
    }

    #[must_use]
    pub const fn language(&self) -> Option<&SubtitleLanguage> {
        self.language.as_ref()
    }

    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    #[must_use]
    pub fn alternatives(&self) -> &[SecretSubtitleUrl] {
        &self.alternatives
    }
}

impl fmt::Debug for SubtitleTrack {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SubtitleTrack")
            .field("id", &self.id)
            .field("language", &self.language)
            .field("label", &"[REDACTED]")
            .field("alternatives", &self.alternatives)
            .finish()
    }
}

pub fn parse_subtitle_fields(wrapper_json: &str) -> Result<Vec<SubtitleTrack>, RezkaError> {
    let fields: SubtitleFields = serde_json::from_str(wrapper_json)
        .map_err(|_| invalid_playback("invalid subtitle response"))?;
    let Some(listing) = fields.subtitle else {
        return Ok(Vec::new());
    };
    let languages = fields.languages.unwrap_or_default();
    parse_listing(&listing, &languages)
}

fn parse_listing(
    listing: &str,
    languages: &BTreeMap<String, SubtitleLanguage>,
) -> Result<Vec<SubtitleTrack>, RezkaError> {
    if listing.is_empty() {
        return Ok(Vec::new());
    }
    let mut tracks = Vec::new();
    'tracks: for entry in listing.split(',') {
        let close = entry
            .find(']')
            .ok_or_else(|| invalid_playback("subtitle track is malformed"))?;
        if !entry.starts_with('[') {
            return Err(invalid_playback("subtitle track is malformed"));
        }
        let label = entry[1..close].trim().to_owned();
        if label.is_empty() || label.len() > 4_096 {
            return Err(invalid_playback("subtitle label is invalid"));
        }
        let raw_urls = entry[close + 1..]
            .split(" or ")
            .map(str::trim)
            .collect::<Vec<_>>();
        if raw_urls.is_empty() || raw_urls.iter().any(|value| value.is_empty()) {
            return Err(invalid_playback("subtitle alternatives are invalid"));
        }
        let mut alternatives = Vec::new();
        for raw in raw_urls {
            let url = Url::parse(raw).map_err(|_| invalid_playback("subtitle URL is invalid"))?;
            let duplicate = alternatives.iter().any(|existing: &SecretSubtitleUrl| {
                existing.with_url(|left| left.as_str() == url.as_str())
            });
            if !duplicate {
                // A non-public or insecure subtitle URL degrades the whole track: skip it and keep
                // the remaining tracks rather than failing the entire manifest.
                match SecretSubtitleUrl::new(url) {
                    Ok(alternative) => alternatives.push(alternative),
                    Err(_) => continue 'tracks,
                }
            }
            if alternatives.len() > MAX_ALTERNATIVES_PER_TRACK {
                return Err(invalid_playback("subtitle alternative limit exceeded"));
            }
        }
        let ordinal = u16::try_from(tracks.len())
            .map_err(|_| invalid_playback("subtitle track limit exceeded"))?;
        let language = languages
            .get(&label)
            .map(|language| SubtitleLanguage(language.0.clone()));
        tracks.push(SubtitleTrack {
            id: SubtitleTrackId {
                provider_label: label.clone(),
                ordinal,
            },
            language,
            label,
            alternatives,
        });
        if tracks.len() > MAX_SUBTITLE_TRACKS {
            return Err(invalid_playback("subtitle track limit exceeded"));
        }
    }
    Ok(tracks)
}

fn normalize_language(value: String) -> Result<SubtitleLanguage, String> {
    let normalized = value
        .trim_matches(|character: char| character.is_ascii_whitespace())
        .replace('_', "-")
        .to_ascii_lowercase();
    let parts = normalized.split('-').collect::<Vec<_>>();
    let valid = (1..=35).contains(&normalized.len())
        && parts.len() <= 4
        && parts.first().is_some_and(|part| {
            (1..=8).contains(&part.len())
                && part.as_bytes()[0].is_ascii_lowercase()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
        && parts.iter().skip(1).all(|part| {
            (1..=8).contains(&part.len())
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        });
    valid
        .then_some(SubtitleLanguage(normalized))
        .ok_or_else(|| "invalid subtitle language".to_owned())
}

#[derive(Default)]
struct SubtitleFields {
    subtitle: Option<String>,
    languages: Option<BTreeMap<String, SubtitleLanguage>>,
}

impl<'de> serde::Deserialize<'de> for SubtitleFields {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_map(SubtitleFieldsVisitor)
    }
}

struct SubtitleFieldsVisitor;

impl<'de> Visitor<'de> for SubtitleFieldsVisitor {
    type Value = SubtitleFields;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a subtitle wrapper object")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut subtitle_seen = false;
        let mut languages_seen = false;
        let mut fields = SubtitleFields::default();
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "subtitle" => {
                    if std::mem::replace(&mut subtitle_seen, true) {
                        return Err(de::Error::duplicate_field("subtitle"));
                    }
                    fields.subtitle = map.next_value::<OptionalString>()?.0;
                }
                "subtitle_lns" => {
                    if std::mem::replace(&mut languages_seen, true) {
                        return Err(de::Error::duplicate_field("subtitle_lns"));
                    }
                    fields.languages = map.next_value::<OptionalLanguages>()?.0;
                }
                _ => {
                    let _: de::IgnoredAny = map.next_value()?;
                }
            }
        }
        Ok(fields)
    }
}

struct OptionalString(Option<String>);

impl<'de> serde::Deserialize<'de> for OptionalString {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(OptionalStringVisitor)
    }
}

struct OptionalStringVisitor;

impl<'de> Visitor<'de> for OptionalStringVisitor {
    type Value = OptionalString;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("false, null, or a subtitle string")
    }

    fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(OptionalString(None))
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(OptionalString(None))
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Self::Value, E> {
        if value {
            Err(E::custom("subtitle boolean must be false"))
        } else {
            Ok(OptionalString(None))
        }
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
        Ok(OptionalString(
            (!value.is_empty()).then(|| value.to_owned()),
        ))
    }

    fn visit_string<E: de::Error>(self, value: String) -> Result<Self::Value, E> {
        Ok(OptionalString((!value.is_empty()).then_some(value)))
    }
}

struct OptionalLanguages(Option<BTreeMap<String, SubtitleLanguage>>);

impl<'de> serde::Deserialize<'de> for OptionalLanguages {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(OptionalLanguagesVisitor)
    }
}

struct OptionalLanguagesVisitor;

impl<'de> Visitor<'de> for OptionalLanguagesVisitor {
    type Value = OptionalLanguages;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("false, null, empty string, or a language object")
    }

    fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(OptionalLanguages(None))
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(OptionalLanguages(None))
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Self::Value, E> {
        if value {
            Err(E::custom("subtitle language boolean must be false"))
        } else {
            Ok(OptionalLanguages(None))
        }
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
        if value.is_empty() {
            Ok(OptionalLanguages(None))
        } else {
            Err(E::custom("subtitle language string must be empty"))
        }
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut languages = BTreeMap::new();
        while let Some(key) = map.next_key::<String>()? {
            let value = map.next_value::<String>()?;
            let language = normalize_language(value).map_err(de::Error::custom)?;
            if languages.insert(key, language).is_some() {
                return Err(de::Error::custom("duplicate subtitle language key"));
            }
        }
        Ok(OptionalLanguages(Some(languages)))
    }
}
