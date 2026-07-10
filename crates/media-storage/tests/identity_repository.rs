mod support;

use media_core::{
    CanonicalEpisode, CanonicalMedia, CanonicalSeason, EpisodeId, EpisodeProviderMapping,
    ExternalNamespace, ExternalReference, IdentityStore, MappingSource, MediaExternalReference,
    MediaId, MediaKind, PortError, Provider, SeasonId, SeriesOrdering,
};
use media_storage::SeaOrmIdentityStore;
use support::{TestDatabase, query};

fn example_series(id: MediaId) -> CanonicalMedia {
    CanonicalMedia::new(
        id,
        MediaKind::Series,
        "Frieren: Beyond Journey's End".to_owned(),
        Some(2023),
        Some(SeriesOrdering::TmdbAired),
    )
    .unwrap()
}

#[tokio::test]
async fn canonical_media_and_external_identity_round_trip_as_domain_types() {
    let test_db = TestDatabase::start_migrated().await;
    let store = SeaOrmIdentityStore::new(test_db.connection().clone());
    let media = example_series(MediaId::new());

    assert_eq!(store.create_media(media.clone()).await.unwrap(), media);
    let reference = MediaExternalReference::new(
        media.id(),
        ExternalReference::new(
            ExternalNamespace::Tmdb,
            "209867".to_owned(),
            MappingSource::ConfirmedByUser,
        )
        .unwrap(),
    );
    assert_eq!(
        store
            .add_external_reference(reference.clone())
            .await
            .unwrap(),
        reference,
    );
    assert_eq!(
        store
            .find_media_by_external_reference(ExternalNamespace::Tmdb, "209867")
            .await
            .unwrap(),
        Some(media),
    );
    assert_eq!(
        store
            .find_media_by_external_reference(ExternalNamespace::Tvdb, "209867")
            .await
            .unwrap(),
        None,
    );
}

#[tokio::test]
async fn external_reference_coordinates_are_globally_unique() {
    let test_db = TestDatabase::start_migrated().await;
    let store = SeaOrmIdentityStore::new(test_db.connection().clone());
    let first = example_series(MediaId::new());
    let second = CanonicalMedia::new(
        MediaId::new(),
        MediaKind::Movie,
        "Different title".to_owned(),
        Some(2024),
        None,
    )
    .unwrap();
    store.create_media(first.clone()).await.unwrap();
    store.create_media(second.clone()).await.unwrap();

    let external = |media_id| {
        MediaExternalReference::new(
            media_id,
            ExternalReference::new(
                ExternalNamespace::Imdb,
                "tt22248376".to_owned(),
                MappingSource::Discovered,
            )
            .unwrap(),
        )
    };
    store
        .add_external_reference(external(first.id()))
        .await
        .unwrap();

    assert_eq!(
        store.add_external_reference(external(second.id())).await,
        Err(PortError::Conflict),
    );
}

#[tokio::test]
async fn season_episode_and_confirmed_provider_mapping_are_persisted() {
    let test_db = TestDatabase::start_migrated().await;
    let store = SeaOrmIdentityStore::new(test_db.connection().clone());
    let media = example_series(MediaId::new());
    let season =
        CanonicalSeason::new(SeasonId::new(), media.id(), 1, Some("Season 1".to_owned())).unwrap();
    let episode = CanonicalEpisode::new(
        EpisodeId::new(),
        season.id(),
        1,
        Some(1),
        Some("The Journey's End".to_owned()),
    )
    .unwrap();
    let mapping = EpisodeProviderMapping::new(
        episode.id(),
        Provider::Rezka,
        "frieren-title".to_owned(),
        1,
        1,
        MappingSource::ConfirmedByUser,
    )
    .unwrap();

    store.create_media(media).await.unwrap();
    assert_eq!(store.create_season(season.clone()).await.unwrap(), season);
    assert_eq!(
        store.create_episode(episode.clone()).await.unwrap(),
        episode,
    );
    assert_eq!(
        store.save_episode_mapping(mapping.clone()).await.unwrap(),
        mapping,
    );

    let persisted = query(
        test_db.connection(),
        "SELECT source, provider, provider_media_ref
         FROM episode_provider_mappings",
    )
    .await;
    assert_eq!(persisted.len(), 1);
    assert_eq!(
        persisted[0].try_get::<String>("", "source").unwrap(),
        "confirmed_by_user",
    );
    assert_eq!(
        persisted[0].try_get::<String>("", "provider").unwrap(),
        "rezka",
    );
    assert_eq!(
        persisted[0]
            .try_get::<String>("", "provider_media_ref")
            .unwrap(),
        "frieren-title",
    );
}
