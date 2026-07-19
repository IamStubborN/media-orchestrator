use std::{
    future::Future,
    sync::{Arc, Mutex},
    task::{Context, Poll, Waker},
};

use media_core::{
    PRIMARY_CLIENT_ID, PRIMARY_USER_ID, Actor, ClientRole, EpisodeSnapshot, NewTrackingCommand,
    NewTrackingSubscription, OperationKey, PortError, Provider, TrackingApplication,
    TrackingDownloadPatch, TrackingId, TrackingScope, TrackingState, TrackingStore,
    TrackingSubscription, SECONDARY_CLIENT_ID, SECONDARY_USER_ID,
};

#[derive(Default)]
struct FakeTrackingStore {
    values: Mutex<Vec<media_core::TrackingSubscription>>,
}

#[async_trait::async_trait]
impl TrackingStore for FakeTrackingStore {
    async fn add(
        &self,
        _: OperationKey,
        value: NewTrackingSubscription,
    ) -> Result<media_core::TrackingSubscription, PortError> {
        let value = value.into_persisted();
        self.values.lock().unwrap().push(value.clone());
        Ok(value)
    }

    async fn list_visible(
        &self,
        user: media_core::UserId,
    ) -> Result<Vec<media_core::TrackingSubscription>, PortError> {
        Ok(self
            .values
            .lock()
            .unwrap()
            .iter()
            .filter(|value| value.is_visible_to(user))
            .cloned()
            .collect())
    }

    async fn patch_download_visible(
        &self,
        id: TrackingId,
        user: media_core::UserId,
        patch: TrackingDownloadPatch,
    ) -> Result<Option<TrackingSubscription>, PortError> {
        let mut values = self.values.lock().unwrap();
        let Some(index) = values
            .iter()
            .position(|value| value.id() == id && value.is_visible_to(user))
        else {
            return Ok(None);
        };
        let value = &values[index];
        let updated = TrackingSubscription::rehydrate(
            value.id(),
            value.owner_id(),
            value.provider(),
            value.title().to_owned(),
            patch.translation().to_owned(),
            value.known_episodes().to_vec(),
            value.scope(),
            Some(patch.download().clone()),
        )
        .map_err(|_| PortError::Conflict)?;
        values[index] = updated.clone();
        Ok(Some(updated))
    }

    async fn remove_visible(
        &self,
        _: OperationKey,
        id: TrackingId,
        user: media_core::UserId,
    ) -> Result<Option<media_core::TrackingSubscription>, PortError> {
        let mut values = self.values.lock().unwrap();
        let index = values
            .iter()
            .position(|value| value.id() == id && value.is_visible_to(user));
        Ok(index.map(|index| values.remove(index)))
    }
}

fn actor_primary() -> Actor {
    Actor::new(PRIMARY_CLIENT_ID, Some(PRIMARY_USER_ID), ClientRole::Hermes).unwrap()
}

fn actor_secondary() -> Actor {
    Actor::new(
        SECONDARY_CLIENT_ID,
        Some(SECONDARY_USER_ID),
        ClientRole::Hermes,
    )
    .unwrap()
}

fn command(scope: TrackingScope) -> NewTrackingCommand {
    NewTrackingCommand {
        provider: Provider::Rezka,
        title: "Ongoing Show".to_owned(),
        translation: "Studio Dub".to_owned(),
        known_episodes: vec![EpisodeSnapshot::new(1, 1).unwrap()],
        scope,
        series_ongoing: true,
        download: None,
    }
}

fn block_on<F: Future>(future: F) -> F::Output {
    let mut context = Context::from_waker(Waker::noop());
    let mut future = Box::pin(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => return output,
            Poll::Pending => std::thread::yield_now(),
        }
    }
}

#[test]
fn personal_tracking_is_owned_and_visible_only_to_authenticated_owner() {
    block_on(async {
        let store = Arc::new(FakeTrackingStore::default());
        let app = TrackingApplication::new(store);

        let created = app
            .add(
                &actor_primary(),
                OperationKey::from_bytes([1; 32]),
                command(TrackingScope::Personal),
            )
            .await
            .unwrap();

        assert_eq!(created.owner_id(), PRIMARY_USER_ID);
        assert_eq!(app.list(&actor_primary()).await.unwrap().len(), 1);
        assert!(app.list(&actor_secondary()).await.unwrap().is_empty());
    });
}

#[test]
fn family_tracking_is_visible_and_removable_by_either_fixed_user() {
    block_on(async {
        let store = Arc::new(FakeTrackingStore::default());
        let app = TrackingApplication::new(store);
        let created = app
            .add(
                &actor_primary(),
                OperationKey::from_bytes([2; 32]),
                command(TrackingScope::Family),
            )
            .await
            .unwrap();

        assert_eq!(
            app.list(&actor_secondary()).await.unwrap(),
            vec![created.clone()]
        );
        assert_eq!(
            app.remove(
                &actor_secondary(),
                OperationKey::from_bytes([3; 32]),
                created.id(),
            )
            .await
            .unwrap(),
            created,
        );
    });
}

#[test]
fn ongoing_series_exposes_choice_prompt_without_enabling_tracking() {
    let command = command(TrackingScope::Personal);

    assert_eq!(command.action_state(), TrackingState::ChoiceNeeded);
}

#[test]
fn known_episode_snapshot_accepts_specials_but_rejects_zero_episode() {
    assert_eq!(EpisodeSnapshot::new(2, 7).unwrap().season(), 2);
    assert_eq!(EpisodeSnapshot::new(2, 7).unwrap().episode(), 7);
    assert_eq!(EpisodeSnapshot::new(0, 7).unwrap().season(), 0);
    assert!(EpisodeSnapshot::new(2, 0).is_err());
}
