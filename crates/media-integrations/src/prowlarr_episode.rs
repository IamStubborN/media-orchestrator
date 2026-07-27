#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) enum EpisodeCoverage {
    Range { season: u32, first: u32, last: u32 },
    Set { season: u32, episodes: Vec<u32> },
}

impl EpisodeCoverage {
    pub(crate) fn parse(title: &str) -> Vec<Self> {
        let normalized = title
            .chars()
            .map(|character| match character {
                '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2212}' => '-',
                _ => character.to_ascii_uppercase(),
            })
            .collect::<String>();
        let bytes = normalized.as_bytes();
        let mut coverage = Vec::new();

        for start in 0..bytes.len() {
            if bytes[start] == b'S'
                && boundary_before(bytes, start)
                && let Some((season, after_season)) = parse_number(bytes, start + 1)
                && bytes.get(after_season) == Some(&b'E')
                && let Some((episode, after_episode)) = parse_number(bytes, after_season + 1)
                && let Some(parsed) =
                    parse_episode_tail(bytes, season, episode, after_episode, b'E')
            {
                push_unique(&mut coverage, parsed);
            }

            if bytes[start].is_ascii_digit()
                && boundary_before(bytes, start)
                && let Some((season, after_season)) = parse_number(bytes, start)
                && bytes.get(after_season) == Some(&b'X')
                && let Some((episode, after_episode)) = parse_number(bytes, after_season + 1)
                && let Some(parsed) =
                    parse_episode_tail(bytes, season, episode, after_episode, b'X')
            {
                push_unique(&mut coverage, parsed);
            }
        }

        coverage
    }

    pub(crate) fn contains(&self, season: u32, episode: u32) -> bool {
        match self {
            Self::Range {
                season: candidate_season,
                first,
                last,
            } => *candidate_season == season && (*first..=*last).contains(&episode),
            Self::Set {
                season: candidate_season,
                episodes,
            } => *candidate_season == season && episodes.contains(&episode),
        }
    }
}

pub(crate) fn series_title_matches(release_title: &str, query_title: &str) -> bool {
    let release = identity_words(release_title);
    let query = strip_query_metadata(identity_words(query_title));
    !query.is_empty()
        && query.len() <= release.len()
        && release
            .windows(query.len())
            .any(|candidate| candidate == query)
}

fn parse_episode_tail(
    bytes: &[u8],
    season: u32,
    first: u32,
    after_first: usize,
    separator: u8,
) -> Option<EpisodeCoverage> {
    if first == 0 {
        return None;
    }

    let mut cursor = after_first;
    let mut episodes = vec![first];
    while bytes.get(cursor) == Some(&separator) {
        let (episode, after_episode) = parse_number(bytes, cursor + 1)?;
        if episode == 0 || episodes.len() >= 64 {
            return None;
        }
        episodes.push(episode);
        cursor = after_episode;
    }
    if episodes.len() > 1 {
        return boundary_after(bytes, cursor).then_some(EpisodeCoverage::Set { season, episodes });
    }

    let range_start = skip_spaces(bytes, after_first);
    if bytes.get(range_start) == Some(&b'-') {
        let mut end_start = skip_spaces(bytes, range_start + 1);
        let mut end_season = season;
        if bytes.get(end_start) == Some(&b'S') {
            let (candidate_season, after_season) = parse_number(bytes, end_start + 1)?;
            if bytes.get(after_season) != Some(&b'E') {
                return None;
            }
            end_season = candidate_season;
            end_start = after_season + 1;
        } else if bytes.get(end_start) == Some(&b'E') {
            end_start += 1;
        }
        let (last, after_last) = parse_number(bytes, end_start)?;
        if end_season != season || last < first || !boundary_after(bytes, after_last) {
            return None;
        }
        return Some(EpisodeCoverage::Range {
            season,
            first,
            last,
        });
    }

    boundary_after(bytes, after_first).then_some(EpisodeCoverage::Range {
        season,
        first,
        last: first,
    })
}

fn parse_number(bytes: &[u8], start: usize) -> Option<(u32, usize)> {
    let mut cursor = start;
    let mut value = 0_u32;
    while let Some(byte) = bytes.get(cursor).copied().filter(u8::is_ascii_digit) {
        value = value.checked_mul(10)?.checked_add(u32::from(byte - b'0'))?;
        cursor += 1;
    }
    (cursor > start).then_some((value, cursor))
}

fn skip_spaces(bytes: &[u8], mut cursor: usize) -> usize {
    while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
        cursor += 1;
    }
    cursor
}

fn boundary_before(bytes: &[u8], start: usize) -> bool {
    start == 0 || !bytes[start - 1].is_ascii_alphanumeric()
}

fn boundary_after(bytes: &[u8], end: usize) -> bool {
    bytes
        .get(end)
        .is_none_or(|byte| !byte.is_ascii_alphanumeric())
}

fn identity_words(value: &str) -> Vec<String> {
    value
        .to_lowercase()
        .chars()
        .map(|character| {
            if character.is_alphanumeric() {
                character
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .map(ToOwned::to_owned)
        .collect()
}

fn strip_query_metadata(mut words: Vec<String>) -> Vec<String> {
    if words.last().is_some_and(|word| {
        word.len() == 4 && word.chars().all(|character| character.is_ascii_digit())
    }) {
        words.pop();
    }
    if words.len() >= 2
        && words
            .last()
            .is_some_and(|word| word.chars().all(|character| character.is_ascii_digit()))
        && words
            .get(words.len() - 2)
            .is_some_and(|word| matches!(word.as_str(), "season" | "сезон" | "tv" | "тв"))
    {
        words.truncate(words.len() - 2);
    } else if words.last().is_some_and(|word| {
        let suffix = word
            .strip_prefix('s')
            .or_else(|| word.strip_prefix("tv"))
            .or_else(|| word.strip_prefix("тв"));
        suffix.is_some_and(|number| {
            !number.is_empty() && number.chars().all(|character| character.is_ascii_digit())
        })
    }) {
        words.pop();
    }
    words
}

fn push_unique(coverage: &mut Vec<EpisodeCoverage>, candidate: EpisodeCoverage) {
    if !coverage.contains(&candidate) {
        coverage.push(candidate);
    }
}

#[cfg(test)]
mod tests {
    use super::{EpisodeCoverage, series_title_matches};

    fn contains(title: &str, season: u32, episode: u32) -> bool {
        EpisodeCoverage::parse(title)
            .iter()
            .any(|coverage| coverage.contains(season, episode))
    }

    #[test]
    fn parses_exact_multi_and_range_coordinates() {
        for title in [
            "Show S03E05",
            "Show 3x05",
            "Show S03E05E06",
            "Show S03E01-06",
            "Show S03E01-E06",
            "Show S03E01-S03E06",
            "Show S3E1-6 of 8",
            "Show 3x01-06",
        ] {
            assert!(contains(title, 3, 5), "{title}");
        }
        assert!(contains("Show S03E05E06", 3, 6));
    }

    #[test]
    fn rejects_coordinates_that_do_not_explicitly_cover_the_episode() {
        assert!(!contains("Show S03E01-06", 3, 7));
        assert!(!contains("Show S02E05", 3, 5));
        assert!(!contains("Show S03 Complete", 3, 5));
        assert!(!contains("Show Episode 5", 3, 5));
        assert!(!contains("Show 3050p", 3, 5));
        assert!(!contains("Show S03E06-S04E02", 3, 6));
    }

    #[test]
    fn matches_series_identity_without_season_or_year_suffixes() {
        assert!(series_title_matches(
            "House.of.the.Dragon.S03E06.1080p",
            "House of the Dragon (2026)"
        ));
        assert!(series_title_matches(
            "Реинкарнация безработного S03E05",
            "Реинкарнация безработного [ТВ-3]"
        ));
        assert!(!series_title_matches(
            "Different Show S03E05",
            "Example Show"
        ));
    }
}
