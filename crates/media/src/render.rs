//! Human-readable rendering of media service responses.
//!
//! The binary is the composition root, so this module parses transport JSON
//! pragmatically with [`serde_json::Value`] instead of the typed
//! `media-contract` DTOs. Renderers read the keys they know about and omit
//! anything that is absent, so a response with extra or missing fields still
//! produces sensible output. The machine contract (the `--json` flag) never
//! goes through this module; it prints the raw response verbatim.

use serde_json::Value;

/// Render a search page: a header identifying the session and an aligned
/// table of results. Columns that no result populates are dropped.
#[must_use]
pub fn search_page(value: &Value) -> String {
    let session = get_str(value, "session_id");
    let source = get_str(value, "source");
    let expires = get_str(value, "expires_at");

    let mut header = String::from("Search");
    if let Some(session) = &session {
        header.push_str(&format!(" session {session}"));
    }
    if let Some(source) = &source {
        header.push_str(&format!(" ({source})"));
    }
    if let Some(expires) = &expires {
        header.push_str(&format!(", expires {expires}"));
    }

    let results = array(value, "results");
    if results.is_empty() {
        return format!("{header}\nNo results.");
    }

    let columns = [
        Column::always("#"),
        Column::always("RESULT"),
        Column::always("TITLE"),
        Column::optional("YEAR"),
        Column::optional("KIND"),
        Column::optional("QUALITY"),
        Column::optional("SIZE"),
        Column::optional("SEEDERS"),
        Column::optional("GROUP"),
        Column::optional("TRANSLATIONS"),
        Column::optional("SEASONS"),
    ];
    let rows = results
        .iter()
        .enumerate()
        .map(|(index, result)| {
            vec![
                Some((index + 1).to_string()),
                get_str(result, "result_id"),
                get_str(result, "title"),
                get_u64(result, "year").map(|year| year.to_string()),
                get_str(result, "media_kind"),
                get_str(result, "quality"),
                get_u64(result, "size_bytes").map(format_size),
                get_i64(result, "seeders").map(|seeders| seeders.to_string()),
                get_str(result, "release_group"),
                translations_summary(result),
                seasons_summary(result),
            ]
        })
        .collect::<Vec<_>>();

    let mut output = format!("{header}\n\n{}", table(&columns, &rows));
    if let Some(continuation) = get_str(value, "continuation") {
        let source = source.unwrap_or_else(|| "<source>".to_owned());
        output.push_str(&format!(
            "\n\nMore results: search {source} --continue {continuation}"
        ));
    }
    output
}

/// Render a single job as a key-value block. `action` labels a confirmation
/// (for example `Created job`); `None` renders a plain `Job` view.
#[must_use]
pub fn job(value: &Value, action: Option<&str>) -> String {
    let id = get_str(value, "id").unwrap_or_else(|| "<unknown>".to_owned());
    let header = match action {
        Some(action) => format!("{action} {id}"),
        None => format!("Job {id}"),
    };

    let mut pairs = Vec::new();
    push_pair(&mut pairs, "State", get_str(value, "state"));
    push_pair(
        &mut pairs,
        "Needs action",
        get_str(value, "needs_action_reason"),
    );
    push_pair(&mut pairs, "Provider", get_str(value, "provider"));
    push_pair(&mut pairs, "Result", get_str(value, "result_ref"));
    push_pair(&mut pairs, "Notify", get_str(value, "notify_scope"));
    push_pair(&mut pairs, "Stage", job_stage(value));
    push_pair(&mut pairs, "Attempts", get_scalar(value, "attempts"));
    push_pair(&mut pairs, "Created", get_str(value, "created_at"));
    push_pair(&mut pairs, "Updated", get_str(value, "updated_at"));

    key_value_block(&header, &pairs)
}

/// Render a job list as an aligned table, or a placeholder when empty.
#[must_use]
pub fn job_list(value: &Value) -> String {
    let jobs = array(value, "jobs");
    if jobs.is_empty() {
        return "No jobs.".to_owned();
    }

    let columns = [
        Column::always("#"),
        Column::always("ID"),
        Column::always("STATE"),
        Column::optional("NEEDS ACTION"),
        Column::optional("PROVIDER"),
        Column::optional("RESULT"),
        Column::optional("STAGE"),
        Column::optional("ATTEMPTS"),
        Column::optional("UPDATED"),
    ];
    let rows = jobs
        .iter()
        .enumerate()
        .map(|(index, job)| {
            vec![
                Some((index + 1).to_string()),
                get_str(job, "id"),
                get_str(job, "state"),
                get_str(job, "needs_action_reason"),
                get_str(job, "provider"),
                get_str(job, "result_ref"),
                job_stage(job),
                get_scalar(job, "attempts"),
                get_str(job, "updated_at"),
            ]
        })
        .collect::<Vec<_>>();

    table(&columns, &rows)
}

/// Render queue status as a key-value block.
#[must_use]
pub fn queue_status(value: &Value) -> String {
    let mut pairs = Vec::new();
    push_pair(
        &mut pairs,
        "Queued",
        get_u64(value, "queued").map(|queued| queued.to_string()),
    );
    push_pair(&mut pairs, "Active", get_bool(value, "active").map(yes_no));
    push_pair(&mut pairs, "Runner", get_str(value, "runner_state"));
    push_pair(
        &mut pairs,
        "Blocked reason",
        get_str(value, "blocked_reason"),
    );
    key_value_block("Queue", &pairs)
}

/// Render a bounded TMDB weekly trending list.
#[must_use]
pub fn trending(value: &Value) -> String {
    let page = get_u64(value, "page").unwrap_or(1);
    let total_pages = get_u64(value, "total_pages");
    let category = get_str(value, "category").unwrap_or_else(|| "all".to_owned());
    let page_label = total_pages.map_or_else(
        || page.to_string(),
        |total_pages| format!("{page}/{total_pages}"),
    );
    let header = format!("TMDB trending this week ({category}), page {page_label}");
    let results = array(value, "results");
    if results.is_empty() {
        return format!("{header}\nNo trending titles found.");
    }
    let lines = results
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let title = get_str(item, "title").unwrap_or_else(|| "<unknown>".to_owned());
            let original = get_str(item, "original_title")
                .map(|value| format!(" / {value}"))
                .unwrap_or_default();
            let year = get_u64(item, "year")
                .map(|value| format!(" ({value})"))
                .unwrap_or_default();
            let kind = get_str(item, "media_type")
                .map(|value| format!(" [{value}]"))
                .unwrap_or_default();
            let rating = item
                .get("rating")
                .and_then(Value::as_f64)
                .map(|value| format!(" - {value:.1}"))
                .unwrap_or_default();
            format!("{}. {title}{original}{year}{kind}{rating}", index + 1)
        })
        .collect::<Vec<_>>();
    format!("{header}\n\n{}", lines.join("\n"))
}

pub fn episode_mapping_action(value: &Value) -> String {
    let title = get_str(value, "title").unwrap_or_else(|| "Unknown title".to_owned());
    let label = get_str(value, "label").unwrap_or_else(|| "Unknown episode".to_owned());
    let provider = value.get("provider").and_then(Value::as_object);
    let season = provider
        .and_then(|value| value.get("season"))
        .and_then(Value::as_u64);
    let episode = provider
        .and_then(|value| value.get("episode"))
        .and_then(Value::as_u64);
    match (season, episode) {
        (Some(season), Some(episode)) => {
            format!("{title}: provider S{season:02}E{episode:02} ({label}) needs canonical mapping")
        }
        _ => format!("{title}: {label} needs canonical episode mapping"),
    }
}

/// Render a single tracking subscription as a key-value block. `action`
/// labels a confirmation (for example `Added tracking`).
#[must_use]
pub fn tracking(value: &Value, action: Option<&str>) -> String {
    let id = get_str(value, "id").unwrap_or_else(|| "<unknown>".to_owned());
    let header = match action {
        Some(action) => format!("{action} {id}"),
        None => format!("Tracking {id}"),
    };

    let mut pairs = Vec::new();
    push_pair(&mut pairs, "State", get_str(value, "state"));
    push_pair(&mut pairs, "Provider", get_str(value, "provider"));
    push_pair(&mut pairs, "Title", get_str(value, "title"));
    push_pair(&mut pairs, "Translation", get_str(value, "translation"));
    push_pair(&mut pairs, "Scope", get_str(value, "scope"));
    push_pair(&mut pairs, "Known episodes", known_episodes_summary(value));

    key_value_block(&header, &pairs)
}

/// Render a tracking list as an aligned table, or a placeholder when empty.
#[must_use]
pub fn tracking_list(value: &Value) -> String {
    let items = array(value, "tracking");
    if items.is_empty() {
        return "No tracking subscriptions.".to_owned();
    }

    let columns = [
        Column::always("#"),
        Column::always("ID"),
        Column::always("STATE"),
        Column::optional("PROVIDER"),
        Column::always("TITLE"),
        Column::optional("TRANSLATION"),
        Column::optional("SCOPE"),
        Column::optional("LATEST"),
    ];
    let rows = items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            vec![
                Some((index + 1).to_string()),
                get_str(item, "id"),
                get_str(item, "state"),
                get_str(item, "provider"),
                get_str(item, "title"),
                get_str(item, "translation"),
                get_str(item, "scope"),
                known_episodes_summary(item),
            ]
        })
        .collect::<Vec<_>>();

    table(&columns, &rows)
}

#[must_use]
pub fn release(value: &Value) -> String {
    let source = get_str(value, "source").unwrap_or_else(|| "unknown".to_owned());
    if get_str(value, "status").as_deref() == Some("choice_needed") {
        let candidates = array(value, "candidates");
        if candidates.is_empty() {
            return format!("No release metadata match found ({source}).");
        }
        let rows = candidates
            .iter()
            .enumerate()
            .map(|(index, candidate)| {
                vec![
                    Some((index + 1).to_string()),
                    get_u64(candidate, "source_id").map(|id| id.to_string()),
                    get_str(candidate, "title"),
                    get_u64(candidate, "year").map(|year| year.to_string()),
                    get_str(candidate, "lifecycle"),
                ]
            })
            .collect::<Vec<_>>();
        return format!(
            "Release choice needed ({source})\n\n{}",
            table(
                &[
                    Column::always("#"),
                    Column::always("SOURCE ID"),
                    Column::always("TITLE"),
                    Column::optional("YEAR"),
                    Column::optional("LIFECYCLE"),
                ],
                &rows
            ),
        );
    }

    let show = value.get("show").unwrap_or(&Value::Null);
    let title = get_str(show, "title").unwrap_or_else(|| "<unknown>".to_owned());
    let mut pairs = Vec::new();
    push_pair(&mut pairs, "Source", Some(source));
    push_pair(&mut pairs, "Lifecycle", get_str(value, "lifecycle"));
    let counts = match (
        get_u64(value, "released_episodes"),
        get_u64(value, "expected_episodes"),
    ) {
        (Some(released), Some(expected)) => Some(format!("{released}/{expected}")),
        (Some(released), None) => Some(released.to_string()),
        _ => None,
    };
    push_pair(&mut pairs, "Released / expected", counts);
    if let Some(next) = value.get("next_episode").filter(|next| !next.is_null()) {
        let episode = match (get_u64(next, "season"), get_u64(next, "episode")) {
            (Some(season), Some(episode)) => Some(format!(
                "S{season:02}E{episode:02} {}",
                get_str(next, "title").unwrap_or_default()
            )),
            _ => None,
        };
        push_pair(&mut pairs, "Next episode", episode);
        push_pair(&mut pairs, "Air time", get_str(next, "air_at"));
        push_pair(&mut pairs, "Precision", get_str(next, "precision"));
    }
    let mut output = key_value_block(&format!("Release schedule for {title}"), &pairs);
    output.push_str("\n\nTVmaze provides schedule metadata, not Rezka availability.");
    output
}

// --- Formatting primitives -------------------------------------------------

/// A table column. `always` columns render even when every cell is empty;
/// optional columns are dropped when no row supplies a value.
struct Column {
    header: &'static str,
    always: bool,
}

impl Column {
    const fn always(header: &'static str) -> Self {
        Self {
            header,
            always: true,
        }
    }

    const fn optional(header: &'static str) -> Self {
        Self {
            header,
            always: false,
        }
    }
}

/// Render an aligned, space-separated table. Each row must have one cell per
/// column, in order. Optional columns with no data are omitted entirely;
/// empty cells in a retained column render as `-`.
fn table(columns: &[Column], rows: &[Vec<Option<String>>]) -> String {
    let kept = (0..columns.len())
        .filter(|&index| columns[index].always || rows.iter().any(|row| row[index].is_some()))
        .collect::<Vec<_>>();

    let headers = kept
        .iter()
        .map(|&index| columns[index].header.to_owned())
        .collect::<Vec<_>>();
    let body = rows
        .iter()
        .map(|row| {
            kept.iter()
                .map(|&index| row[index].clone().unwrap_or_else(|| "-".to_owned()))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();

    let mut widths = headers
        .iter()
        .map(|header| width(header))
        .collect::<Vec<_>>();
    for row in &body {
        for (column, cell) in row.iter().enumerate() {
            widths[column] = widths[column].max(width(cell));
        }
    }

    let mut lines = Vec::with_capacity(body.len() + 1);
    lines.push(render_row(&headers, &widths));
    for row in &body {
        lines.push(render_row(row, &widths));
    }
    lines.join("\n")
}

fn render_row(cells: &[String], widths: &[usize]) -> String {
    cells
        .iter()
        .enumerate()
        .map(|(column, cell)| format!("{cell:<width$}", width = widths[column]))
        .collect::<Vec<_>>()
        .join("  ")
        .trim_end()
        .to_owned()
}

/// Render a header line followed by an indented, colon-aligned key-value
/// block. Empty blocks render as just the header.
fn key_value_block(header: &str, pairs: &[(&str, String)]) -> String {
    let mut output = header.to_owned();
    let label_width = pairs
        .iter()
        .map(|(label, _)| width(label) + 1)
        .max()
        .unwrap_or(0);
    for (label, value) in pairs {
        let label = format!("{label}:");
        output.push_str(&format!("\n  {label:<label_width$} {value}"));
    }
    output
}

fn push_pair<'a>(pairs: &mut Vec<(&'a str, String)>, label: &'a str, value: Option<String>) {
    if let Some(value) = value {
        pairs.push((label, value));
    }
}

fn width(text: &str) -> usize {
    text.chars().count()
}

fn yes_no(value: bool) -> String {
    if value { "yes" } else { "no" }.to_owned()
}

/// Format a byte count with binary units. Sub-kibibyte values keep exact
/// byte counts; larger values use one decimal place.
fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

fn translations_summary(result: &Value) -> Option<String> {
    let translations = result.get("translations")?.as_array()?;
    if translations.is_empty() {
        return None;
    }
    Some(translations.len().to_string())
}

fn seasons_summary(result: &Value) -> Option<String> {
    let seasons = result.get("availability")?.get("seasons")?.as_array()?;
    if seasons.is_empty() {
        return None;
    }
    Some(format!("{} seasons", seasons.len()))
}

fn known_episodes_summary(value: &Value) -> Option<String> {
    let episodes = value.get("known_episodes")?.as_array()?;
    let mut latest: Option<(u64, u64)> = None;
    for episode in episodes {
        let season = episode.get("season").and_then(Value::as_u64).unwrap_or(0);
        let number = episode.get("episode").and_then(Value::as_u64).unwrap_or(0);
        if latest.is_none_or(|current| (season, number) > current) {
            latest = Some((season, number));
        }
    }
    let (season, number) = latest?;
    Some(format!("S{season}E{number} ({} known)", episodes.len()))
}

/// Job stage is not part of the current contract; read a couple of plausible
/// keys so the field appears if the service ever includes it.
fn job_stage(value: &Value) -> Option<String> {
    get_scalar(value, "current_stage").or_else(|| get_scalar(value, "stage"))
}

// --- JSON accessors --------------------------------------------------------

fn present<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    value.get(key).filter(|found| !found.is_null())
}

fn get_str(value: &Value, key: &str) -> Option<String> {
    present(value, key)
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn get_u64(value: &Value, key: &str) -> Option<u64> {
    present(value, key).and_then(Value::as_u64)
}

fn get_i64(value: &Value, key: &str) -> Option<i64> {
    present(value, key).and_then(Value::as_i64)
}

fn get_bool(value: &Value, key: &str) -> Option<bool> {
    present(value, key).and_then(Value::as_bool)
}

/// Read a scalar of unknown JSON type as display text. Useful for fields
/// whose contract type is not fixed (for example stage or attempts).
fn get_scalar(value: &Value, key: &str) -> Option<String> {
    match present(value, key)? {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

fn array<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

#[cfg(test)]
mod tests {
    use super::{job, job_list, queue_status, release, search_page, tracking, tracking_list};
    use serde_json::json;

    #[test]
    fn queue_status_renders_aligned_block() {
        let rendered = queue_status(&json!({
            "queued": 3,
            "active": false,
            "runner_state": "blocked",
            "blocked_reason": "vpn_rotation_failed"
        }));
        assert_eq!(
            rendered,
            "Queue\n  Queued:         3\n  Active:         no\n  Runner:         blocked\n  Blocked reason: vpn_rotation_failed"
        );
    }

    #[test]
    fn queue_status_omits_absent_fields() {
        let rendered = queue_status(&json!({ "active": true }));
        assert_eq!(rendered, "Queue\n  Active: yes");
    }

    #[test]
    fn job_renders_key_value_block_with_aligned_labels() {
        let rendered = job(
            &json!({
                "id": "018f3f86-7b4c-7b4f-9b6a-6d62f45bb111",
                "provider": "rezka",
                "result_ref": "item",
                "state": "queued",
                "notify_scope": "initiator"
            }),
            None,
        );
        assert_eq!(
            rendered,
            "Job 018f3f86-7b4c-7b4f-9b6a-6d62f45bb111\n  \
             State:    queued\n  \
             Provider: rezka\n  \
             Result:   item\n  \
             Notify:   initiator"
        );
    }

    #[test]
    fn job_confirmation_uses_action_header() {
        let rendered = job(
            &json!({ "id": "job-1", "state": "queued" }),
            Some("Created job"),
        );
        assert_eq!(rendered, "Created job job-1\n  State: queued");
    }

    #[test]
    fn job_shows_needs_action_reason_when_present() {
        let rendered = job(
            &json!({
                "id": "job-1",
                "state": "needs_action",
                "needs_action_reason": "plex_mismatch"
            }),
            None,
        );
        assert!(rendered.contains("Needs action: plex_mismatch"));
    }

    #[test]
    fn job_renders_optional_stage_attempts_and_timestamps_when_present() {
        let rendered = job(
            &json!({
                "id": "job-1",
                "state": "running",
                "current_stage": "downloading",
                "attempts": 2,
                "created_at": "2026-07-12T10:00:00Z",
                "updated_at": "2026-07-12T10:05:00Z"
            }),
            None,
        );
        assert!(rendered.contains("Stage:    downloading"));
        assert!(rendered.contains("Attempts: 2"));
        assert!(rendered.contains("Created:  2026-07-12T10:00:00Z"));
        assert!(rendered.contains("Updated:  2026-07-12T10:05:00Z"));
    }

    #[test]
    fn job_list_reports_empty() {
        assert_eq!(job_list(&json!({ "jobs": [] })), "No jobs.");
    }

    #[test]
    fn job_list_renders_table_and_drops_unused_columns() {
        let rendered = job_list(&json!({
            "jobs": [
                {
                    "id": "018f3f86-7b4c-7b4f-9b6a-6d62f45bb111",
                    "provider": "rezka",
                    "result_ref": "item",
                    "state": "queued",
                    "notify_scope": "initiator"
                }
            ]
        }));
        let lines = rendered.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 2);
        let header = lines[0];
        assert!(header.contains("STATE"));
        assert!(header.contains("PROVIDER"));
        assert!(header.contains("RESULT"));
        // Unused optional columns are dropped.
        assert!(!header.contains("NEEDS ACTION"));
        assert!(!header.contains("STAGE"));
        assert!(!header.contains("UPDATED"));
        let row = lines[1];
        assert!(row.starts_with("1  018f3f86-7b4c-7b4f-9b6a-6d62f45bb111"));
        assert!(row.contains("queued"));
        assert!(row.contains("rezka"));
        assert!(row.contains("item"));
    }

    #[test]
    fn job_list_keeps_needs_action_column_when_any_row_has_it() {
        let rendered = job_list(&json!({
            "jobs": [
                { "id": "a", "state": "queued" },
                {
                    "id": "b",
                    "state": "needs_action",
                    "needs_action_reason": "identity_ambiguous"
                }
            ]
        }));
        assert!(rendered.contains("NEEDS ACTION"));
        assert!(rendered.contains("identity_ambiguous"));
    }

    #[test]
    fn tracking_confirmation_renders_summary() {
        let rendered = tracking(
            &json!({
                "id": "018f3f86-7b4c-7b4f-9b6a-6d62f45bb111",
                "provider": "rezka",
                "title": "Ongoing Show",
                "translation": "Studio Dub",
                "known_episodes": [{ "season": 1, "episode": 4 }],
                "scope": "family",
                "state": "active"
            }),
            Some("Added tracking"),
        );
        assert!(rendered.starts_with("Added tracking 018f3f86-7b4c-7b4f-9b6a-6d62f45bb111"));
        assert!(rendered.contains("Title:          Ongoing Show"));
        assert!(rendered.contains("Translation:    Studio Dub"));
        assert!(rendered.contains("Scope:          family"));
        assert!(rendered.contains("Known episodes: S1E4 (1 known)"));
    }

    #[test]
    fn tracking_list_reports_empty() {
        assert_eq!(
            tracking_list(&json!({ "tracking": [] })),
            "No tracking subscriptions."
        );
    }

    #[test]
    fn tracking_list_renders_latest_known_episode() {
        let rendered = tracking_list(&json!({
            "tracking": [
                {
                    "id": "track-1",
                    "provider": "rezka",
                    "title": "Ongoing Show",
                    "translation": "Studio Dub",
                    "known_episodes": [
                        { "season": 1, "episode": 4 },
                        { "season": 2, "episode": 1 }
                    ],
                    "scope": "family",
                    "state": "active"
                }
            ]
        }));
        assert!(rendered.contains("LATEST"));
        assert!(rendered.contains("S2E1 (2 known)"));
        assert!(rendered.contains("Ongoing Show"));
    }

    #[test]
    fn search_page_renders_prowlarr_results_with_size_and_seeders() {
        let rendered = search_page(&json!({
            "api_version": "v1",
            "session_id": "session-1",
            "source": "prowlarr",
            "expires_at": "2026-07-13T12:00:00Z",
            "continuation": "session-1:5",
            "results": [
                {
                    "source": "prowlarr",
                    "result_id": "result-1",
                    "title": "Movie",
                    "size_bytes": 2_147_483_648u64,
                    "seeders": 12,
                    "release_group": "GRP"
                }
            ]
        }));
        assert!(
            rendered
                .starts_with("Search session session-1 (prowlarr), expires 2026-07-13T12:00:00Z")
        );
        assert!(rendered.contains("SIZE"));
        assert!(rendered.contains("SEEDERS"));
        assert!(rendered.contains("2.0 GiB"));
        assert!(rendered.contains("GRP"));
        assert!(rendered.contains("More results: search prowlarr --continue session-1:5"));
        // Prowlarr rows have no year, so the YEAR column is dropped.
        assert!(!rendered.contains("YEAR"));
    }

    #[test]
    fn search_page_renders_rezka_results_with_year_and_availability() {
        let rendered = search_page(&json!({
            "api_version": "v1",
            "session_id": "session-2",
            "source": "rezka",
            "expires_at": "2026-07-13T12:00:00Z",
            "results": [
                {
                    "source": "rezka",
                    "result_id": "rezka-1",
                    "title": "Series",
                    "year": 2021,
                    "media_kind": "series",
                    "translations": [
                        { "id": 1, "name": "A", "premium": false, "director": false, "camrip": false, "has_ads": false },
                        { "id": 2, "name": "B", "premium": false, "director": false, "camrip": false, "has_ads": false }
                    ],
                    "availability": {
                        "lifecycle_status": "ongoing",
                        "incomplete": false,
                        "seasons": [
                            { "season": 1, "episodes": [1, 2] },
                            { "season": 2, "episodes": [1] }
                        ]
                    }
                }
            ]
        }));
        assert!(rendered.contains("YEAR"));
        assert!(rendered.contains("2021"));
        assert!(rendered.contains("series"));
        assert!(rendered.contains("TRANSLATIONS"));
        assert!(rendered.contains("2 seasons"));
        // No continuation was supplied.
        assert!(!rendered.contains("More results"));
        // Rezka rows carry no seeders, so that column is dropped.
        assert!(!rendered.contains("SEEDERS"));
    }

    #[test]
    fn search_page_reports_no_results() {
        let rendered = search_page(&json!({
            "api_version": "v1",
            "session_id": "session-3",
            "source": "prowlarr",
            "expires_at": "2026-07-13T12:00:00Z",
            "results": []
        }));
        assert!(rendered.ends_with("No results."));
    }

    #[test]
    fn release_human_output_does_not_expand_full_schedule() {
        let schedule = (1..=1_000)
            .map(|episode| {
                json!({
                    "source_id": episode,
                    "season": 1,
                    "episode": episode,
                    "title": format!("Episode {episode}"),
                    "air_at": "2099-01-01",
                    "precision": "date"
                })
            })
            .collect::<Vec<_>>();
        let rendered = release(&json!({
            "status": "matched",
            "source": "tvmaze",
            "show": { "title": "Long Show" },
            "lifecycle": "ongoing",
            "released_episodes": 10,
            "expected_episodes": 1000,
            "next_episode": null,
            "schedule": schedule
        }));

        assert!(rendered.len() < 500);
        assert!(!rendered.contains("Episode 1000"));
    }
}
