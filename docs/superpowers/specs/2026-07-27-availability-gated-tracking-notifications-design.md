# Availability-Gated Tracking Notifications

## Goal

Notify a user about a released episode only after at least one configured media
source confirms that the exact episode can be downloaded.

The release calendar remains an internal scheduling signal. A calendar entry by
itself must never create a Telegram message.

## Current Behavior

Release-calendar tracking treats an aired episode as discovered immediately:

1. TVmaze reports that the episode has aired.
2. The episode is appended to `known_episodes`.
3. A `media.source-choice` notification is written with the fixed actions
   `all`, `rezka`, and `prowlarr`.
4. Availability is checked only after the user presses a button.

This produces noisy messages when neither source has the episode. It also makes
the calendar claim stronger than the evidence: "aired" does not mean
"downloadable".

## Decision

Use an availability-gated tracking flow:

1. The release calendar produces episode candidates.
2. `media-service` checks the exact candidate in Rezka and Prowlarr without
   sending a notification.
3. A candidate with no confirmed source remains unknown to `known_episodes` and
   is checked again on the next tracking run.
4. A candidate is recorded and announced only after one or both providers
   confirm an exact downloadable result.
5. Telegram actions contain only confirmed providers:
   - both providers: `All | Rezka | Prowlarr`;
   - Rezka only: `Rezka`;
   - Prowlarr only: `Prowlarr`.

There is no automatic fallback and no download is created by the availability
probe.

## Scope

This change applies to manual release-calendar tracking that currently produces
source-choice notifications.

Existing Rezka automatic-download tracking already waits for the selected
translation and episode to appear on Rezka before creating a job. Its behavior
remains unchanged.

## Domain Model

Introduce an episode availability boundary separate from release metadata:

```text
EpisodeAvailabilityPort
  probe(request) -> EpisodeAvailability

EpisodeAvailabilityRequest
  tracking
  episode
  release title
  original release title

EpisodeAvailability
  rezka: ProviderAvailability
  prowlarr: ProviderAvailability

ProviderAvailability
  Available
  Unavailable
  Unknown
```

`Unknown` represents a provider error, timeout, disabled indexers, or an
ambiguous response. It is not equivalent to `Unavailable`.

The notification action list is derived from `Available` providers. It must be
non-empty and deterministic. `All` is included only when both providers are
available.

## Provider Probes

### Rezka

The Rezka probe reuses the existing catalog search and translation episode
metadata. It confirms availability only when the selected title contains the
exact season and episode in at least one downloadable translation.

An empty successful result is `Unavailable`. Authentication, transport,
anti-bot, or parsing failures are `Unknown`.

### Prowlarr

The Prowlarr probe uses exact TV-search parameters for every enabled indexer:

```text
t=tvsearch
q=<query variant>
season=<season number>
ep=<episode number>
```

This uses Prowlarr's Newznab/Torznab-compatible indexer route. Indexers do not
consistently honor the exact episode parameters: some return cumulative packs
such as `S3E1-6 of 8`, and some omit the structured Newznab coordinates.
Therefore the response is verified with a hybrid matcher:

1. the release title must identify the queried series;
2. structured `season` and `episode` attributes are used when both are present;
3. the release title is parsed into exact, multi-episode, or bounded-range
   coverage;
4. conflicting structured attributes and title coverage reject the item;
5. a bare season pack or absolute anime number without an explicit mapping does
   not confirm availability.

A range confirms only episodes it actually contains. For example,
`S3E1-6 of 8` confirms `S03E06` but not `S03E07`.

The probe first uses the original title returned by the matched release
metadata. If it is absent, it uses the matched release title. The localized
tracking title is retained as a final distinct query variant. A result from any
variant confirms availability; duplicate variants are not queried twice.

The aggregate result is:

- `Available` when any enabled indexer returns at least one usable torrent;
- `Unavailable` when all queried, enabled indexers respond successfully and
  none returns a usable torrent;
- `Unknown` when no result is found and at least one enabled indexer cannot be
  checked.

A usable torrent must expose a download link or enclosure required by the
existing selection flow and pass the episode-coverage verification above.

The normal interactive Prowlarr search remains unchanged.

## Tracking Flow

For each due release-calendar subscription:

1. Load all aired calendar episodes.
2. Retain the matched release title and original title as availability query
   metadata.
3. Remove episodes already present in `known_episodes`.
4. Sort the remaining candidates by season and episode.
5. Probe Rezka and Prowlarr concurrently for each candidate.
6. If no provider is `Available`, emit nothing and leave the candidate out of
   `known_episodes`.
7. If at least one provider is `Available`, atomically:
   - insert the unique `tracking_discoveries` row;
   - append the episode to `known_episodes`;
   - enqueue one source-choice notification per recipient with the derived
     actions.
8. Schedule the next tracking check.

The runtime must compare candidates against the complete `known_episodes` set,
not only the maximum known episode. This allows a later episode to be announced
without permanently losing an earlier episode that becomes downloadable later.

The existing unique constraint on
`tracking_discoveries(tracking_id, season, episode)` remains the concurrency and
notification deduplication boundary. No new pending-candidate table is needed:
the release calendar deterministically reproduces unrecorded aired candidates.

Release-calendar subscriptions use the following fixed cadence:

- 30 minutes while at least one aired candidate has no confirmed source;
- 6 hours when there are no unrecorded aired candidates.

Automatic Rezka download tracking keeps its existing 15-minute cadence.

## Notification Contract

`SourceChoiceNotification.actions` changes from a fixed three-element array to
a validated non-empty collection.

Valid action sets are exactly:

```text
[rezka]
[prowlarr]
[all, rezka, prowlarr]
```

The webhook schema remains `media.source-choice` version 1 because the field is
already a list and consumers already parse individual actions. The notifier and
Telegram plugin must render only the actions present in the payload.

No message is created for an empty action set.

## Failure Handling

Provider failures are silent user-facing events. They are recorded in structured
service logs with the provider, tracking ID, season, episode, and stable error
category.

An availability probe failure must not:

- append the episode to `known_episodes`;
- insert a discovery row;
- create or update a Telegram card;
- create a download job;
- substitute one provider for another.

The subscription remains scheduled and retries after 30 minutes. Manual search
remains available independently.

## Prowlarr Health

The current live incident was not an empty search. All enabled indexers entered
Prowlarr's temporary disabled state after network failures through the shared
VPN. The Prowlarr container remained healthy while its search API returned:

```text
Search failed due to all selected indexers being unavailable
```

The availability probe must classify this response as `Unknown`, never
`Unavailable`. A later successful indexer check can make the same episode
`Available` without any lost state.

## Compatibility

Previously delivered calendar-only messages are historical and are not removed.

A data migration repairs pending, undelivered calendar-only notifications:

1. mark the outbox row dead with `availability_unverified`;
2. remove the corresponding episode from `known_episodes`;
3. remove the corresponding `tracking_discoveries` row.

The episode then returns as an unrecorded calendar candidate and passes through
the new availability gate. Delivered source-choice rows and their discoveries
remain unchanged.

No API used by Hermes changes. Inline callback authorization, user scope, search
pagination, explicit source selection, and explicit download selection remain
unchanged.

## Verification

### Rust

- Calendar-only candidates do not create a discovery or notification.
- Rezka-only availability records the episode with the `rezka` action.
- Prowlarr-only availability records the episode with the `prowlarr` action.
- Dual availability records actions in `all`, `rezka`, `prowlarr` order.
- Provider errors without another available provider remain silent and retryable.
- One provider error plus availability in the other provider still announces
  only the confirmed provider.
- A missing earlier episode remains eligible after a later episode is recorded.
- Concurrent tracking runs create one discovery and one notification per
  recipient.
- Automatic Rezka download tracking keeps its existing behavior.

### Hermes

- Source-choice cards render one horizontal row containing only payload actions.
- Single-provider cards do not render `All` or the unavailable provider.
- Existing three-action cards remain compatible.

### Live QA

1. Confirm Prowlarr indexers and Rezka are reachable.
2. Run a synthetic calendar candidate unavailable in both providers and verify
   that Telegram receives no message.
3. Make the candidate available in one provider and rerun tracking.
4. Verify that exactly one card appears with exactly one provider button.
5. Press the button and verify that the normal search results are returned
   without creating a download.
6. Repeat with both providers and verify the single-row
   `All | Rezka | Prowlarr` card.

## Acceptance Criteria

- Users receive no message merely because an episode aired.
- Every new-episode card corresponds to at least one confirmed downloadable
  provider.
- Cards expose only providers confirmed for that exact episode.
- Provider outages do not cause false "not available" decisions or lost
  episodes.
- No availability probe downloads media or invokes an LLM.
