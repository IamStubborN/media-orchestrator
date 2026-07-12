use std::sync::Arc;

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum ReleasePrecision {
    Date,
    DateTime,
    Unknown,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum ReleaseLifecycle {
    Ongoing,
    Ended,
    Upcoming,
    Unknown,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ReleaseQuery {
    pub title: String,
    pub original_title: Option<String>,
    pub year: Option<i32>,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, thiserror::Error)]
pub enum ReleaseQueryError {
    #[error("release title cannot be empty")]
    EmptyTitle,
    #[error("original release title cannot be empty")]
    EmptyOriginalTitle,
    #[error("release metadata provider failed")]
    Provider,
}

impl ReleaseQuery {
    pub fn new(
        title: impl Into<String>,
        original_title: Option<String>,
        year: Option<i32>,
    ) -> Result<Self, ReleaseQueryError> {
        let title = title.into();
        if title.trim().is_empty() {
            return Err(ReleaseQueryError::EmptyTitle);
        }
        if original_title
            .as_ref()
            .is_some_and(|value| value.trim().is_empty())
        {
            return Err(ReleaseQueryError::EmptyOriginalTitle);
        }
        Ok(Self {
            title,
            original_title,
            year,
        })
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ReleaseCandidate {
    pub source_id: u64,
    pub title: String,
    pub original_title: Option<String>,
    pub year: Option<i32>,
    pub lifecycle: ReleaseLifecycle,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ScheduledEpisode {
    pub source_id: u64,
    pub season: u32,
    pub episode: u32,
    pub title: String,
    pub air_at: Option<String>,
    pub precision: ReleasePrecision,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ReleaseMetadataResult {
    Matched {
        source: String,
        fetched_at: String,
        show: ReleaseCandidate,
        precision: ReleasePrecision,
        lifecycle: ReleaseLifecycle,
        released_episodes: u32,
        expected_episodes: Option<u32>,
        next_episode: Option<ScheduledEpisode>,
        schedule: Vec<ScheduledEpisode>,
    },
    ChoiceNeeded {
        source: String,
        fetched_at: String,
        candidates: Vec<ReleaseCandidate>,
    },
}

#[async_trait::async_trait]
pub trait ReleaseMetadataPort: Send + Sync {
    async fn query(&self, query: &ReleaseQuery)
    -> Result<ReleaseMetadataResult, ReleaseQueryError>;
}

pub struct ReleaseMetadataService {
    provider: Arc<dyn ReleaseMetadataPort>,
}

impl ReleaseMetadataService {
    #[must_use]
    pub fn new(provider: Arc<dyn ReleaseMetadataPort>) -> Self {
        Self { provider }
    }

    pub async fn query(
        &self,
        query: ReleaseQuery,
    ) -> Result<ReleaseMetadataResult, ReleaseQueryError> {
        self.provider.query(&query).await
    }
}

#[must_use]
pub fn select_release_candidate(
    query: &ReleaseQuery,
    candidates: &[ReleaseCandidate],
) -> Option<usize> {
    let titles = [Some(query.title.as_str()), query.original_title.as_deref()];
    let matches = candidates
        .iter()
        .enumerate()
        .filter(|(_, candidate)| {
            query.year.is_none_or(|year| candidate.year == Some(year))
                && titles.iter().flatten().any(|title| {
                    candidate.title.eq_ignore_ascii_case(title)
                        || candidate
                            .original_title
                            .as_deref()
                            .is_some_and(|value| value.eq_ignore_ascii_case(title))
                })
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();

    if matches.len() == 1 {
        Some(matches[0])
    } else {
        None
    }
}
