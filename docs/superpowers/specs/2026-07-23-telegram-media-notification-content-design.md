# Telegram Media Notification Content Design

## Status

Approved on 2026-07-23.

This specification extends the lifecycle, delivery, routing, and ownership rules in
`2026-07-20-telegram-notification-lifecycle-design.md`. It supersedes that document's
presentation, copy, and active-card update cadence where they differ. The existing one-card
lifecycle, revision ordering, final push, and no-LLM delivery model remain unchanged.

## Goal

Make every media notification immediately understandable while keeping routine progress
quiet. The user must be able to identify the exact movie, episode, or season; understand
what is happening; see trustworthy final media characteristics; and recover from common
failures without seeing internal identifiers or implementation details.

## Non-Goals

- Do not use an LLM to create, update, or complete automatic notification cards.
- Do not expose stream URLs, staging paths, cookies, tokens, hashes, or raw command output.
- Do not invent percentages, completion estimates, codecs, languages, or processing results.
- Do not create one Telegram message per episode for a manually requested season download.
- Do not change active job ownership, queue ordering, provider fallback, or download policy.

## Notification Model

### Tracked Episode

A newly discovered episode owns one card from discovery through Plex publication. When
automatic downloading is enabled, the first card combines discovery and download startup:

```text
🆕 Найдена новая серия

Клинки Хранителей · S02E08
🎙 AniLibria · Rezka
⬇️ Автоматическое скачивание началось
```

The same card is edited during transfer and processing. Routine edits are sent directly
through the Telegram adapter no more frequently than once every five seconds when values
change, normally producing one update every 5-10 seconds. Stage transitions are immediate.
Updates do not invoke the agent and do not create additional chat messages.

### Single-Episode Request

A manually requested episode uses the same exact episode identity, for example `S02E08`.
It must never be rendered only as `Season 2`.

### Season Request

A manually requested season owns one aggregate card. The card reports completed episodes,
the current episode, transfer measurements, and missing episodes. It does not create a full
card for every episode:

```text
⬇️ Магия и мускулы · Сезон 1

📺 Готово: 5 из 12 серий
🔄 Сейчас: S01E06 · скачивание
📦 184 МБ · 5,8 МБ/с
⚠️ Ошибок: нет
```

Absolute provider episode numbers and the number of episodes in the job are separate
values. For example, `current_episode=8` and `total_episodes=1` is valid for a one-episode
job and must render as `S02E08`.

## Card States

### Downloading

The active card shows the exact media identity, selected source and translation, downloaded
bytes, speed, and season progress. Percentage and ETA are shown only when the source
provides a trustworthy total size.

```text
⬇️ Скачивается

Клинки Хранителей · S02E08
📦 286 МБ · 6,4 МБ/с
📚 Сезон: готово 7 из 12 серий
🔄 Получение исходного видео
```

### Processing

The processing card distinguishes completed transfer from media preparation:

```text
⚙️ Подготовка для Plex

Клинки Хранителей · S02E08
✅ Видео скачано
🔄 VAAPI upscale до 1080p
📚 Сезон: готово 7 из 12 серий
```

`VAAPI upscale` may appear only after the runner reports that exact operation. A generic
completed job is not evidence that an upscale occurred.

### Completed

The final card uses the approved detailed presentation:

```text
✅ Загрузка и обработка завершены

Клинки Хранителей · S02E08
Источник: Rezka · перевод: AniLibria
Видео: 1920x1080 · HEVC Main
Аудио: русский · AAC Stereo
Субтитры: 2 дорожки
Размер: 420 МБ · длительность: 23:41
Обработка: VAAPI upscale · 4 мин 12 сек
Plex → Сериалы → Клинки Хранителей → Сезон 2
```

Unavailable fields are omitted rather than replaced with assumptions. The existing card
is edited into this final state. Telegram does not send push notifications for message
edits, so Hermes sends one short reply:

```text
✅ Клинки Хранителей · S02E08 уже в Plex
```

The short reply contains no duplicate technical details.

### Partial Season

A season with at least one published episode and at least one failed episode is a partial
success:

```text
⚠️ Сезон загружен частично

📺 В Plex: 11 из 12 серий
❌ Не удалось: S01E07
✅ Остальные серии готовы
```

The card offers `Повторить S01E07`, `Выбрать другой источник`, and `Диагностика`. Retrying
missing work must not redownload completed episodes.

## Trustworthy Final Metadata

The download runner is the source of truth for file characteristics. After producing the
final file, it probes the artifact and emits sanitized structured metadata through the
existing job event/checkpoint path:

- video codec, profile, width, and height;
- duration and final file size;
- audio language, codec, channel layout, and track title when available;
- downloaded and missing subtitle track counts;
- actual processing mode and elapsed processing time;
- Plex media kind, canonical title, season, episode, and publication destination.

The runner must extend its current probe only where a required field is not already
available. The event must not contain a filesystem path or provider URL.

Media-service persists the measured values with the job result and projects them into the
structured notification payload. Hermes performs deterministic Russian rendering. It must
not infer final characteristics from a requested quality, provider label, or completed
status.

### Provider-Specific Rules

- Rezka reports VAAPI upscale only when the operation completed and the final probe confirms
  the artifact. Requested `1080p` is not treated as measured `1920x1080`.
- Prowlarr and qBittorrent content is reported as `without transcoding`. File characteristics
  are displayed only when the published artifact is available for a trustworthy probe.
- Missing subtitles do not fail a usable video. The final result becomes partial when one or
  more expected subtitle tracks failed.

## Retry And Failure Presentation

Retry activity updates the existing card. It does not produce one message per attempt:

```text
⏳ Восстанавливаем загрузку

Магия и мускулы · S01E07
Rezka временно прервала передачу видео
🔄 Попытка соединения: 5 из 20
🌐 VPN будет сменён перед следующей попыткой

Ничего делать не нужно
```

The phrase `distribution active: 5 of 20` must not be used. A retry counter is not download
progress. The displayed connection-attempt counter is a curated user-facing value; raw
provider attempts and internal retry details remain hidden.

After automatic attempts are exhausted, the card becomes actionable:

```text
❌ Не удалось скачать серию

Магия и мускулы · S01E07
Rezka не смогла передать видео после повторных попыток
Остальные серии сезона сохранены
```

Common failures have specific user-facing behavior:

- an expired Rezka session is refreshed automatically before user action is requested;
- insufficient storage reports required and currently available space;
- a Plex publication failure preserves the prepared file and retries publication only;
- incomplete subtitles produce a partial result rather than discarding the video;
- a transient source or VPN failure reports recovery activity without raw provider errors.

Primary cards do not expose job IDs, internal stage names, stack traces, command lines, or
codes such as `execution_failed`. The `Diagnostics` action may display a sanitized job ID,
error code, attempt history, and a concise technical cause.

## Inline Actions And Search Responses

Media actions use real Telegram inline keyboards attached to the relevant message. The LLM
must not emit pseudo-control markup such as `<telegram-quick-replies>`.

The Hermes media integration attaches structured callback actions directly through the
Telegram Bot API. It authorizes callbacks by Telegram user and media ownership before
calling media-service. Deterministic callbacks do not invoke the conversational model.

Search results remain useful when one provider fails. A successful Rezka result is shown
even when Prowlarr is temporarily unavailable:

```text
🔎 Найдено на Rezka

Вместе до конца / Гони или умри
Сериал · 2026 · 8 серий · 9 озвучек

[Выбрать Rezka] [Показать озвучки]
[Повторить поиск в Prowlarr]
```

Normal Telegram responses must not show:

- raw `<telegram-quick-replies>` tags;
- shell commands such as `hermes-media search`;
- repeated internal provider errors;
- standalone UUIDs or job identifiers.

Technical diagnostics remain available through an explicit action or direct diagnostic
request.

## Data And Component Responsibilities

```text
download-runner
  → final probe, subtitle outcome, processing result, Plex publication result
  → sanitized job checkpoint/result
media-service
  → persisted source of truth and structured notification projection
Hermes media adapter
  → deterministic Russian card and Telegram inline keyboard
Telegram
  → one mutable card plus one short terminal push
```

The existing notification revision, lifecycle cycle, outbox deduplication, routing, and
terminal locking rules remain authoritative. A delayed progress event must never overwrite
a completed or failed card.

## Deployment Safety

Notification schema additions are optional and backward-compatible. Older pending outbox
rows and active jobs remain renderable with the fields they already contain. Missing new
metadata produces a smaller truthful card.

Deployment must not restart, cancel, migrate, or rewrite active downloads. Synthetic
notifications are verified before a real low-volume media job is used.

## Acceptance Criteria

- A tracked episode reports discovery and automatic download startup in one card.
- A single-episode card displays the exact season and episode.
- A season download owns one aggregate card and reports current and completed episodes.
- Active progress edits the same card without invoking an LLM or adding chat messages.
- The detailed final card contains only final-probe and publication facts.
- Rezka upscale is displayed only when actually completed and confirmed.
- Prowlarr content never falsely claims VAAPI processing.
- Retry status is clearly labeled as a connection attempt, not download progress.
- Partial seasons preserve completed episodes and offer retry for missing work only.
- Job IDs and internal errors appear only through explicit diagnostics.
- Search actions use real inline buttons on Telegram mobile and Web.
- Raw quick-reply tags, shell commands, and standalone UUIDs do not appear in normal chat.
- Under acknowledged Telegram delivery, the final card edit is followed by exactly one
  short terminal push. The existing ambiguous-timeout limitation remains unchanged.

## Verification

Automated coverage includes:

- contract serialization and backward compatibility for optional final metadata;
- final-probe projection and omission of unavailable values;
- single-episode absolute numbering and season aggregation;
- deterministic downloading, processing, completed, partial, and failed rendering;
- terminal locking, card revision ordering, and final-push deduplication;
- inline callback ownership and stale-action rejection;
- suppression of raw quick-reply markup and internal identifiers.

End-to-end verification covers:

1. synthetic cards for every state and both providers;
2. inline actions in Telegram mobile and Web Telegram;
3. one real tracked or explicitly requested low-volume Rezka episode through Plex;
4. comparison of the final card with `ffprobe`, subtitle files, and Plex placement;
5. one controlled retry or failure path without notification spam;
6. removal of test media and staging artifacts after verification.
