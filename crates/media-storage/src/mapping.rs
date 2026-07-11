use media_core::{
    Actor, CanonicalEpisode, CanonicalMedia, CanonicalSeason, ClientRole, EpisodeId,
    EpisodeProviderMapping, ExternalNamespace, ExternalReference, Job, JobState, MappingSource,
    MediaExternalReference, MediaId, MediaKind, NeedsActionReason, NewJob, NotifyScope, Provider,
    SeasonId, SeriesOrdering,
};
use sea_orm::{ActiveValue::Set, prelude::Uuid};

use crate::entity::{
    api_client, episode, episode_provider_mapping, job, media, media_external_ref, season,
};

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum MappingError {
    InvalidPersistedValue,
    NumericOverflow,
}

impl TryFrom<api_client::Model> for Actor {
    type Error = MappingError;

    fn try_from(model: api_client::Model) -> Result<Self, Self::Error> {
        let role = match model.role.as_str() {
            "hermes" => ClientRole::Hermes,
            "runner" => ClientRole::Runner,
            _ => return Err(MappingError::InvalidPersistedValue),
        };
        Actor::new(
            media_core::ClientId::from_uuid(model.id),
            model.user_id.map(media_core::UserId::from_uuid),
            role,
        )
        .map_err(|_| MappingError::InvalidPersistedValue)
    }
}

pub(crate) fn client_active_model(client: &media_core::BootstrapClient) -> api_client::ActiveModel {
    let now = time::OffsetDateTime::now_utc();
    api_client::ActiveModel {
        id: Set(client.client_id().into_uuid()),
        name: Set(client.name().to_owned()),
        role: Set(client_role_value(client.role()).to_owned()),
        user_id: Set(client.user_id().map(media_core::UserId::into_uuid)),
        credential_digest: Set(client.digest().as_bytes().to_vec()),
        enabled: Set(true),
        created_at: Set(now),
        updated_at: Set(now),
    }
}

pub(crate) const fn client_role_value(role: ClientRole) -> &'static str {
    match role {
        ClientRole::Hermes => "hermes",
        ClientRole::Runner => "runner",
    }
}

impl TryFrom<media::Model> for CanonicalMedia {
    type Error = MappingError;

    fn try_from(model: media::Model) -> Result<Self, Self::Error> {
        let kind = match model.kind.as_str() {
            "movie" => MediaKind::Movie,
            "series" => MediaKind::Series,
            _ => return Err(MappingError::InvalidPersistedValue),
        };
        let ordering = model
            .series_ordering
            .as_deref()
            .map(parse_series_ordering)
            .transpose()?;
        CanonicalMedia::new(
            MediaId::from_uuid(model.id),
            kind,
            model.title,
            model.release_year,
            ordering,
        )
        .map_err(|_| MappingError::InvalidPersistedValue)
    }
}

pub(crate) fn media_active_model(value: &CanonicalMedia) -> media::ActiveModel {
    let now = time::OffsetDateTime::now_utc();
    media::ActiveModel {
        id: Set(value.id().into_uuid()),
        kind: Set(media_kind_value(value.kind()).to_owned()),
        title: Set(value.title().to_owned()),
        release_year: Set(value.release_year()),
        series_ordering: Set(value
            .ordering()
            .map(series_ordering_value)
            .map(str::to_owned)),
        metadata_snapshot: Set(serde_json::json!({})),
        created_at: Set(now),
        updated_at: Set(now),
    }
}

impl TryFrom<media_external_ref::Model> for MediaExternalReference {
    type Error = MappingError;

    fn try_from(model: media_external_ref::Model) -> Result<Self, Self::Error> {
        let namespace = parse_external_namespace(&model.namespace)?;
        let source = parse_mapping_source(&model.source)?;
        let reference = ExternalReference::new(namespace, model.value, source)
            .map_err(|_| MappingError::InvalidPersistedValue)?;
        Ok(Self::new(MediaId::from_uuid(model.media_id), reference))
    }
}

pub(crate) fn external_reference_active_model(
    value: &MediaExternalReference,
) -> media_external_ref::ActiveModel {
    let now = time::OffsetDateTime::now_utc();
    media_external_ref::ActiveModel {
        id: Set(Uuid::new_v4()),
        media_id: Set(value.media_id().into_uuid()),
        namespace: Set(external_namespace_value(value.reference().namespace()).to_owned()),
        value: Set(value.reference().value().to_owned()),
        source: Set(mapping_source_value(value.reference().source()).to_owned()),
        provider_snapshot: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    }
}

impl TryFrom<season::Model> for CanonicalSeason {
    type Error = MappingError;

    fn try_from(model: season::Model) -> Result<Self, Self::Error> {
        CanonicalSeason::new(
            SeasonId::from_uuid(model.id),
            MediaId::from_uuid(model.media_id),
            u32::try_from(model.season_number).map_err(|_| MappingError::NumericOverflow)?,
            model.title,
        )
        .map_err(|_| MappingError::InvalidPersistedValue)
    }
}

pub(crate) fn season_active_model(
    value: &CanonicalSeason,
) -> Result<season::ActiveModel, MappingError> {
    let now = time::OffsetDateTime::now_utc();
    Ok(season::ActiveModel {
        id: Set(value.id().into_uuid()),
        media_id: Set(value.media_id().into_uuid()),
        season_number: Set(
            i32::try_from(value.season_number()).map_err(|_| MappingError::NumericOverflow)?
        ),
        title: Set(value.title().map(str::to_owned)),
        metadata_snapshot: Set(serde_json::json!({})),
        created_at: Set(now),
        updated_at: Set(now),
    })
}

impl TryFrom<episode::Model> for CanonicalEpisode {
    type Error = MappingError;

    fn try_from(model: episode::Model) -> Result<Self, Self::Error> {
        CanonicalEpisode::new(
            EpisodeId::from_uuid(model.id),
            SeasonId::from_uuid(model.season_id),
            u32::try_from(model.episode_number).map_err(|_| MappingError::NumericOverflow)?,
            model
                .absolute_number
                .map(u32::try_from)
                .transpose()
                .map_err(|_| MappingError::NumericOverflow)?,
            model.title,
        )
        .map_err(|_| MappingError::InvalidPersistedValue)
    }
}

pub(crate) fn episode_active_model(
    value: &CanonicalEpisode,
) -> Result<episode::ActiveModel, MappingError> {
    let now = time::OffsetDateTime::now_utc();
    Ok(episode::ActiveModel {
        id: Set(value.id().into_uuid()),
        season_id: Set(value.season_id().into_uuid()),
        episode_number: Set(
            i32::try_from(value.episode_number()).map_err(|_| MappingError::NumericOverflow)?
        ),
        absolute_number: Set(value
            .absolute_number()
            .map(i32::try_from)
            .transpose()
            .map_err(|_| MappingError::NumericOverflow)?),
        title: Set(value.title().map(str::to_owned)),
        metadata_snapshot: Set(serde_json::json!({})),
        created_at: Set(now),
        updated_at: Set(now),
    })
}

impl TryFrom<episode_provider_mapping::Model> for EpisodeProviderMapping {
    type Error = MappingError;

    fn try_from(model: episode_provider_mapping::Model) -> Result<Self, Self::Error> {
        EpisodeProviderMapping::new(
            EpisodeId::from_uuid(model.episode_id),
            parse_provider(&model.provider)?,
            model.provider_media_ref,
            u32::try_from(model.provider_season_number)
                .map_err(|_| MappingError::NumericOverflow)?,
            u32::try_from(model.provider_episode_number)
                .map_err(|_| MappingError::NumericOverflow)?,
            parse_mapping_source(&model.source)?,
        )
        .map_err(|_| MappingError::InvalidPersistedValue)
    }
}

pub(crate) fn episode_mapping_active_model(
    value: &EpisodeProviderMapping,
) -> Result<episode_provider_mapping::ActiveModel, MappingError> {
    let now = time::OffsetDateTime::now_utc();
    Ok(episode_provider_mapping::ActiveModel {
        id: Set(Uuid::new_v4()),
        episode_id: Set(value.episode_id().into_uuid()),
        provider: Set(provider_value(value.provider()).to_owned()),
        provider_media_ref: Set(value.provider_media_ref().to_owned()),
        provider_season_number: Set(i32::try_from(value.provider_season_number())
            .map_err(|_| MappingError::NumericOverflow)?),
        provider_episode_number: Set(i32::try_from(value.provider_episode_number())
            .map_err(|_| MappingError::NumericOverflow)?),
        source: Set(mapping_source_value(value.source()).to_owned()),
        provider_snapshot: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    })
}

impl TryFrom<job::Model> for Job {
    type Error = MappingError;

    fn try_from(model: job::Model) -> Result<Self, Self::Error> {
        Job::rehydrate(
            media_core::JobId::from_uuid(model.id),
            media_core::UserId::from_uuid(model.owner_id),
            parse_provider(&model.provider)?,
            model.result_ref,
            parse_job_state(&model.state)?,
            model
                .needs_action_reason
                .as_deref()
                .map(parse_needs_action_reason)
                .transpose()?,
            parse_notify_scope(&model.notify_scope)?,
        )
        .map_err(|_| MappingError::InvalidPersistedValue)
    }
}

pub(crate) fn job_active_model(value: &NewJob) -> job::ActiveModel {
    let now = time::OffsetDateTime::now_utc();
    job::ActiveModel {
        id: Set(value.id().into_uuid()),
        owner_id: Set(value.owner_id().into_uuid()),
        provider: Set(provider_value(value.provider()).to_owned()),
        result_ref: Set(value.result_ref().to_owned()),
        state: Set(job_state_value(JobState::Queued).to_owned()),
        needs_action_reason: Set(None),
        notify_scope: Set(notify_scope_value(value.notify_scope()).to_owned()),
        request_snapshot: Set(serde_json::json!({})),
        error_snapshot: Set(None),
        attempt_count: Set(0),
        created_at: Set(now),
        updated_at: Set(now),
        started_at: Set(None),
        completed_at: Set(None),
    }
}

pub(crate) const fn external_namespace_value(value: ExternalNamespace) -> &'static str {
    match value {
        ExternalNamespace::Tmdb => "tmdb",
        ExternalNamespace::Tvdb => "tvdb",
        ExternalNamespace::Imdb => "imdb",
        ExternalNamespace::AniList => "anilist",
        ExternalNamespace::Rezka => "rezka",
        ExternalNamespace::Plex => "plex",
        ExternalNamespace::ProwlarrResult => "prowlarr_result",
    }
}

fn parse_external_namespace(value: &str) -> Result<ExternalNamespace, MappingError> {
    match value {
        "tmdb" => Ok(ExternalNamespace::Tmdb),
        "tvdb" => Ok(ExternalNamespace::Tvdb),
        "imdb" => Ok(ExternalNamespace::Imdb),
        "anilist" => Ok(ExternalNamespace::AniList),
        "rezka" => Ok(ExternalNamespace::Rezka),
        "plex" => Ok(ExternalNamespace::Plex),
        "prowlarr_result" => Ok(ExternalNamespace::ProwlarrResult),
        _ => Err(MappingError::InvalidPersistedValue),
    }
}

const fn mapping_source_value(value: MappingSource) -> &'static str {
    match value {
        MappingSource::Discovered => "discovered",
        MappingSource::ConfirmedByUser => "confirmed_by_user",
    }
}

fn parse_mapping_source(value: &str) -> Result<MappingSource, MappingError> {
    match value {
        "discovered" => Ok(MappingSource::Discovered),
        "confirmed_by_user" => Ok(MappingSource::ConfirmedByUser),
        _ => Err(MappingError::InvalidPersistedValue),
    }
}

const fn media_kind_value(value: MediaKind) -> &'static str {
    match value {
        MediaKind::Movie => "movie",
        MediaKind::Series => "series",
    }
}

const fn series_ordering_value(value: SeriesOrdering) -> &'static str {
    match value {
        SeriesOrdering::TmdbAired => "tmdb_aired",
        SeriesOrdering::TvdbAired => "tvdb_aired",
        SeriesOrdering::TvdbDvd => "tvdb_dvd",
        SeriesOrdering::TvdbAbsolute => "tvdb_absolute",
    }
}

fn parse_series_ordering(value: &str) -> Result<SeriesOrdering, MappingError> {
    match value {
        "tmdb_aired" => Ok(SeriesOrdering::TmdbAired),
        "tvdb_aired" => Ok(SeriesOrdering::TvdbAired),
        "tvdb_dvd" => Ok(SeriesOrdering::TvdbDvd),
        "tvdb_absolute" => Ok(SeriesOrdering::TvdbAbsolute),
        _ => Err(MappingError::InvalidPersistedValue),
    }
}

pub(crate) const fn provider_value(value: Provider) -> &'static str {
    match value {
        Provider::Rezka => "rezka",
        Provider::Prowlarr => "prowlarr",
    }
}

pub(crate) fn parse_provider(value: &str) -> Result<Provider, MappingError> {
    match value {
        "rezka" => Ok(Provider::Rezka),
        "prowlarr" => Ok(Provider::Prowlarr),
        _ => Err(MappingError::InvalidPersistedValue),
    }
}

pub(crate) const fn job_state_value(value: JobState) -> &'static str {
    match value {
        JobState::Queued => "queued",
        JobState::Leased => "leased",
        JobState::Running => "running",
        JobState::CancelRequested => "cancel_requested",
        JobState::BlockedStorage => "blocked_storage",
        JobState::Publishing => "publishing",
        JobState::PlexPending => "plex_pending",
        JobState::NeedsAction => "needs_action",
        JobState::Partial => "partial",
        JobState::Completed => "completed",
        JobState::Failed => "failed",
        JobState::Cancelled => "cancelled",
    }
}

pub(crate) fn parse_job_state(value: &str) -> Result<JobState, MappingError> {
    match value {
        "queued" => Ok(JobState::Queued),
        "leased" => Ok(JobState::Leased),
        "running" => Ok(JobState::Running),
        "cancel_requested" => Ok(JobState::CancelRequested),
        "blocked_storage" => Ok(JobState::BlockedStorage),
        "publishing" => Ok(JobState::Publishing),
        "plex_pending" => Ok(JobState::PlexPending),
        "needs_action" => Ok(JobState::NeedsAction),
        "partial" => Ok(JobState::Partial),
        "completed" => Ok(JobState::Completed),
        "failed" => Ok(JobState::Failed),
        "cancelled" => Ok(JobState::Cancelled),
        _ => Err(MappingError::InvalidPersistedValue),
    }
}

pub(crate) const fn notify_scope_value(value: NotifyScope) -> &'static str {
    match value {
        NotifyScope::Initiator => "initiator",
        NotifyScope::Family => "family",
    }
}

pub(crate) fn parse_notify_scope(value: &str) -> Result<NotifyScope, MappingError> {
    match value {
        "initiator" => Ok(NotifyScope::Initiator),
        "family" => Ok(NotifyScope::Family),
        _ => Err(MappingError::InvalidPersistedValue),
    }
}

pub(crate) const fn needs_action_reason_value(value: NeedsActionReason) -> &'static str {
    match value {
        NeedsActionReason::IdentityAmbiguous => "identity_ambiguous",
        NeedsActionReason::PlexMismatch => "plex_mismatch",
    }
}

pub(crate) fn parse_needs_action_reason(value: &str) -> Result<NeedsActionReason, MappingError> {
    match value {
        "identity_ambiguous" => Ok(NeedsActionReason::IdentityAmbiguous),
        "plex_mismatch" => Ok(NeedsActionReason::PlexMismatch),
        _ => Err(MappingError::InvalidPersistedValue),
    }
}
