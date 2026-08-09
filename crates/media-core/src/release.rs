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

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum ReleaseSource {
    Tvmaze,
}

impl ReleaseSource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tvmaze => "tvmaze",
        }
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct ReleaseIdentity {
    source: ReleaseSource,
    source_id: u64,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, thiserror::Error)]
pub enum ReleaseIdentityError {
    #[error("release source id must be greater than zero")]
    ZeroSourceId,
}

impl ReleaseIdentity {
    pub const fn new(source: ReleaseSource, source_id: u64) -> Result<Self, ReleaseIdentityError> {
        if source_id == 0 {
            return Err(ReleaseIdentityError::ZeroSourceId);
        }
        Ok(Self { source, source_id })
    }

    #[must_use]
    pub const fn source(self) -> ReleaseSource {
        self.source
    }

    #[must_use]
    pub const fn source_id(self) -> u64 {
        self.source_id
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ReleaseQuery {
    pub title: String,
    pub original_title: Option<String>,
    pub year: Option<i32>,
    pub source_id: Option<u64>,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, thiserror::Error)]
pub enum ReleaseQueryError {
    #[error("release title cannot be empty")]
    EmptyTitle,
    #[error("original release title cannot be empty")]
    EmptyOriginalTitle,
    #[error("release source id must be greater than zero")]
    ZeroSourceId,
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
            source_id: None,
        })
    }

    pub fn with_source_id(mut self, source_id: u64) -> Result<Self, ReleaseQueryError> {
        if source_id == 0 {
            return Err(ReleaseQueryError::ZeroSourceId);
        }
        self.source_id = Some(source_id);
        Ok(self)
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ReleaseCandidate {
    pub source_id: u64,
    pub title: String,
    pub original_title: Option<String>,
    pub year: Option<i32>,
    pub poster_url: Option<String>,
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
        return Some(matches[0]);
    }

    let ongoing = matches
        .into_iter()
        .filter(|index| candidates[*index].lifecycle == ReleaseLifecycle::Ongoing)
        .collect::<Vec<_>>();

    if ongoing.len() == 1 {
        ongoing.first().copied()
    } else {
        None
    }
}
