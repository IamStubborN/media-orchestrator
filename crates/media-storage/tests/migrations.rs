mod support;

use std::collections::{BTreeMap, BTreeSet};

use media_storage::Migrator;
use sea_orm_migration::MigratorTrait;
use support::{TestDatabase, assert_rejected, execute, query};

const PRIMARY_ID: &str = "00000000-0000-0000-0000-000000000001";
const SECONDARY_ID: &str = "00000000-0000-0000-0000-000000000002";

const APPLICATION_TABLES: [&str; 23] = [
    "api_clients",
    "episode_provider_mappings",
    "episodes",
    "idempotency_records",
    "job_events",
    "job_leases",
    "job_stages",
    "job_tasks",
    "jobs",
    "media",
    "media_external_refs",
    "notification_outbox",
    "operation_receipts",
    "outbox_events",
    "runner_lifecycle",
    "search_executions",
    "search_sessions",
    "seasons",
    "tracking_availability_candidates",
    "tracking_discoveries",
    "tracking_download_reservations",
    "tracking_subscriptions",
    "users",
];

#[tokio::test]
async fn migrations_apply_seed_fixed_users_and_require_backup_for_identity_rollback() {
    let test_db = TestDatabase::start().await;
    let db = test_db.connection();

    Migrator::up(db, None)
        .await
        .expect("all explicit migrations must apply");

    let tables = query(
        db,
        "SELECT table_name
         FROM information_schema.tables
         WHERE table_schema = 'public'
           AND table_type = 'BASE TABLE'
           AND table_name <> 'seaql_migrations'
         ORDER BY table_name",
    )
    .await
    .into_iter()
    .map(|row| row.try_get::<String>("", "table_name").unwrap())
    .collect::<Vec<_>>();
    assert_eq!(tables, APPLICATION_TABLES);

    let users = query(
        db,
        "SELECT id::text AS id, slug
         FROM users
         ORDER BY id",
    )
    .await
    .into_iter()
    .map(|row| {
        (
            row.try_get::<String>("", "id").unwrap(),
            row.try_get::<String>("", "slug").unwrap(),
        )
    })
    .collect::<Vec<_>>();
    assert_eq!(
        users,
        [
            (PRIMARY_ID.to_owned(), "primary".to_owned()),
            (SECONDARY_ID.to_owned(), "secondary".to_owned()),
        ]
    );

    let client_count = query(db, "SELECT count(*)::bigint AS count FROM api_clients")
        .await
        .pop()
        .unwrap()
        .try_get::<i64>("", "count")
        .unwrap();
    assert_eq!(client_count, 0, "migrations must never seed API tokens");

    let uuid_id_tables = query(
        db,
        "SELECT table_name
         FROM information_schema.columns
         WHERE table_schema = 'public'
           AND column_name = 'id'
           AND udt_name = 'uuid'
         ORDER BY table_name",
    )
    .await
    .into_iter()
    .map(|row| row.try_get::<String>("", "table_name").unwrap())
    .collect::<BTreeSet<_>>();
    assert_eq!(
        uuid_id_tables,
        APPLICATION_TABLES
            .into_iter()
            .filter(|table| {
                !matches!(
                    *table,
                    "search_executions"
                        | "runner_lifecycle"
                        | "tracking_availability_candidates"
                        | "tracking_download_reservations"
                )
            })
            .map(str::to_owned)
            .collect()
    );

    let non_timestamptz_timestamps = query(
        db,
        "SELECT table_name, column_name
         FROM information_schema.columns
         WHERE table_schema = 'public'
           AND table_name <> 'seaql_migrations'
           AND (column_name LIKE '%_at' OR column_name = 'created_at')
           AND data_type <> 'timestamp with time zone'",
    )
    .await;
    assert!(
        non_timestamptz_timestamps.is_empty(),
        "every persisted timestamp must be timestamptz"
    );

    let jsonb_columns = query(
        db,
        "SELECT table_name || '.' || column_name AS name
         FROM information_schema.columns
         WHERE table_schema = 'public' AND udt_name = 'jsonb'
         ORDER BY name",
    )
    .await
    .into_iter()
    .map(|row| row.try_get::<String>("", "name").unwrap())
    .collect::<BTreeSet<_>>();
    let expected_jsonb = [
        "episode_provider_mappings.provider_snapshot",
        "episodes.metadata_snapshot",
        "job_stages.checkpoint",
        "job_stages.error_snapshot",
        "job_events.payload",
        "job_tasks.checkpoint",
        "job_tasks.error_snapshot",
        "jobs.error_snapshot",
        "jobs.request_snapshot",
        "media.metadata_snapshot",
        "media_external_refs.provider_snapshot",
        "operation_receipts.result_snapshot",
        "notification_outbox.payload",
        "outbox_events.payload",
        "search_executions.payload",
        "search_sessions.payload",
        "seasons.metadata_snapshot",
        "tracking_subscriptions.known_episodes",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<BTreeSet<_>>();
    assert_eq!(jsonb_columns, expected_jsonb);

    let notification_generation = query(
        db,
        "SELECT column_default, is_nullable
         FROM information_schema.columns
         WHERE table_schema = 'public'
           AND table_name = 'notification_outbox'
           AND column_name = 'generation'",
    )
    .await;
    assert_eq!(notification_generation.len(), 1);
    assert_eq!(
        notification_generation[0]
            .try_get::<String>("", "column_default")
            .unwrap(),
        "1"
    );
    assert_eq!(
        notification_generation[0]
            .try_get::<String>("", "is_nullable")
            .unwrap(),
        "NO"
    );

    let tracking_claim_columns = query(
        db,
        "SELECT column_name, data_type
         FROM information_schema.columns
         WHERE table_schema = 'public'
           AND table_name = 'tracking_subscriptions'
           AND column_name IN ('check_claim_token', 'check_claim_until', 'check_requested_at')
         ORDER BY column_name",
    )
    .await
    .into_iter()
    .map(|row| {
        (
            row.try_get::<String>("", "column_name").unwrap(),
            row.try_get::<String>("", "data_type").unwrap(),
        )
    })
    .collect::<Vec<_>>();
    assert_eq!(
        tracking_claim_columns,
        [
            ("check_claim_token".to_owned(), "uuid".to_owned()),
            (
                "check_claim_until".to_owned(),
                "timestamp with time zone".to_owned(),
            ),
            (
                "check_requested_at".to_owned(),
                "timestamp with time zone".to_owned(),
            ),
        ]
    );

    let generation_constraint = query(
        db,
        "SELECT pg_get_constraintdef(oid) AS definition
         FROM pg_constraint
         WHERE conname = 'notification_generation_positive'",
    )
    .await;
    assert_eq!(generation_constraint.len(), 1);
    assert!(
        generation_constraint[0]
            .try_get::<String>("", "definition")
            .unwrap()
            .contains("generation > 0")
    );

    let event_type_constraint = query(
        db,
        "SELECT pg_get_constraintdef(oid) AS definition
         FROM pg_constraint
         WHERE conname = 'notification_event_type_check'",
    )
    .await;
    assert_eq!(event_type_constraint.len(), 1);
    assert!(
        event_type_constraint[0]
            .try_get::<String>("", "definition")
            .unwrap()
            .contains("download-progress")
    );

    let indexes = query(
        db,
        "SELECT indexname, indexdef
         FROM pg_indexes
         WHERE schemaname = 'public'
           AND indexname = ANY (ARRAY[
             'media_external_refs_media_id_idx',
             'episode_provider_mappings_episode_id_idx',
             'jobs_owner_created_at_idx',
             'jobs_state_created_at_idx',
             'job_tasks_episode_id_idx',
             'idempotency_records_expires_at_idx',
             'job_leases_expires_at_idx'
           ])
         ORDER BY indexname",
    )
    .await
    .into_iter()
    .map(|row| {
        (
            row.try_get::<String>("", "indexname").unwrap(),
            row.try_get::<String>("", "indexdef").unwrap(),
        )
    })
    .collect::<BTreeMap<_, _>>();
    let expected_indexes = [
        ("episode_provider_mappings_episode_id_idx", "(episode_id)"),
        ("idempotency_records_expires_at_idx", "(expires_at)"),
        ("job_leases_expires_at_idx", "(expires_at)"),
        ("job_tasks_episode_id_idx", "(episode_id)"),
        ("jobs_owner_created_at_idx", "(owner_id, created_at)"),
        ("jobs_state_created_at_idx", "(state, created_at)"),
        ("media_external_refs_media_id_idx", "(media_id)"),
    ];
    assert_eq!(
        indexes.keys().map(String::as_str).collect::<Vec<_>>(),
        expected_indexes
            .iter()
            .map(|(name, _)| *name)
            .collect::<Vec<_>>()
    );
    for (name, columns) in expected_indexes {
        assert!(
            indexes[name].contains(columns),
            "index {name} must cover {columns}, got: {}",
            indexes[name]
        );
    }

    let error = Migrator::down(db, Some(1)).await.unwrap_err();
    assert!(error.to_string().contains("database backup"));
    assert_eq!(
        Migrator::get_applied_migrations(db).await.unwrap().len(),
        44
    );
}

#[tokio::test]
async fn earlier_migrations_reverse_cleanly() {
    let test_db = TestDatabase::start().await;
    let db = test_db.connection();
    Migrator::up(db, Some(43)).await.unwrap();
    Migrator::down(db, None).await.unwrap();
    assert!(
        Migrator::get_applied_migrations(db)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_failed_migration_explicitly_rolls_back_partial_schema() {
    let migrations = Migrator::migrations();
    let names = migrations
        .iter()
        .map(|migration| migration.name().to_string())
        .collect::<Vec<_>>();
    assert!(
        names.windows(2).any(|pair| {
            pair == [
                "m20260712_000009_blocked_storage_notification",
                "m20260712_000010_search_scope",
            ]
        }),
        "search scope migration must follow blocked-storage notification support",
    );
    assert_eq!(
        names.last().map(String::as_str),
        Some("m20260929_000044_neutral_user_names"),
        "identity transition must remain the latest schema change",
    );
    for migration in migrations {
        assert_eq!(
            migration.use_transaction(),
            Some(true),
            "each migration must explicitly include its ledger write in an outer transaction"
        );
    }

    let test_db = TestDatabase::start().await;
    let db = test_db.connection();
    execute(db, "CREATE TABLE api_clients (id integer PRIMARY KEY)")
        .await
        .unwrap();

    let error = Migrator::up(db, None)
        .await
        .expect_err("the conflicting api_clients table must fail the first migration");
    assert!(error.to_string().contains("api_clients"));

    let users_count = query(
        db,
        "SELECT count(*)::bigint AS count
         FROM information_schema.tables
         WHERE table_schema = 'public' AND table_name = 'users'",
    )
    .await
    .pop()
    .unwrap()
    .try_get::<i64>("", "count")
    .unwrap();
    assert_eq!(
        users_count, 0,
        "the users table created before the failure must be rolled back"
    );
    assert!(
        Migrator::get_applied_migrations(db)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn search_scope_migration_discards_only_legacy_unscoped_sessions() {
    let test_db = TestDatabase::start().await;
    let db = test_db.connection();
    Migrator::up(db, Some(9)).await.unwrap();
    execute(
        db,
        "INSERT INTO search_sessions (id, owner_id, payload, expires_at) VALUES
         ('10000000-0000-0000-0000-000000000091',
          '00000000-0000-0000-0000-000000000001',
          '{\"request\":{\"source\":\"rezka\",\"query\":\"legacy\"},\"results\":[]}',
          now() + interval '1 hour'),
         ('10000000-0000-0000-0000-000000000092',
          '00000000-0000-0000-0000-000000000001',
          '{\"request\":{\"scope\":{\"platform\":\"telegram\",\"chat_id\":\"42\"},\"source\":\"rezka\",\"query\":\"current\"},\"results\":[]}',
          now() + interval '1 hour')",
    )
    .await
    .unwrap();

    Migrator::up(db, None).await.unwrap();

    let sessions = query(db, "SELECT id FROM search_sessions ORDER BY id").await;
    assert_eq!(sessions.len(), 1);
    assert_eq!(
        sessions[0].try_get::<uuid::Uuid>("", "id").unwrap(),
        uuid::Uuid::parse_str("10000000-0000-0000-0000-000000000092").unwrap()
    );
}

#[tokio::test]
async fn structured_notifications_migration_preserves_legacy_rows_and_enforces_v2_payloads() {
    let test_db = TestDatabase::start().await;
    let db = test_db.connection();
    Migrator::up(db, Some(23)).await.unwrap();

    execute(
        db,
        "INSERT INTO jobs (id, owner_id, provider, result_ref, state, notify_scope)
         VALUES ('00000000-0000-0000-0000-000000000999',
                 '00000000-0000-0000-0000-000000000001', 'rezka', 'example', 'queued', 'initiator');
         INSERT INTO notification_outbox
           (id, aggregate_type, aggregate_id, event_type, recipient, source_dedupe_key, payload)
         VALUES ('00000000-0000-0000-0000-000000000998', 'job',
                 '00000000-0000-0000-0000-000000000999', 'started', 'primary',
                 decode(repeat('01', 32), 'hex'), '{\"message\":\"legacy notification\"}'::jsonb)",
    )
    .await
    .unwrap();

    Migrator::up(db, None).await.unwrap();

    let cycle = query(
        db,
        "SELECT notification_cycle FROM jobs WHERE id = '00000000-0000-0000-0000-000000000999'",
    )
    .await;
    assert_eq!(
        cycle[0].try_get::<i64>("", "notification_cycle").unwrap(),
        1
    );

    let preserved = query(
        db,
        "SELECT payload FROM notification_outbox WHERE id = '00000000-0000-0000-0000-000000000998'",
    )
    .await;
    assert_eq!(
        preserved.len(),
        1,
        "legacy outbox rows must not be rewritten"
    );

    execute(
        db,
        "INSERT INTO notification_outbox
           (id, aggregate_type, aggregate_id, event_type, recipient, source_dedupe_key, payload)
         VALUES ('00000000-0000-0000-0000-000000000997', 'job',
                 '00000000-0000-0000-0000-000000000999', 'download-progress', 'primary',
                 decode(repeat('02', 32), 'hex'),
                 '{
                   \"event_type\": \"media.notification\",
                   \"schema_version\": 2,
                   \"delivery_kind\": \"card\",
                   \"card_key\": \"media-job:00000000-0000-0000-0000-000000000999\",
                   \"revision\": 7,
                   \"lifecycle_cycle\": 1,
                   \"terminal\": false,
                   \"state\": \"downloading\",
                   \"media\": {
                     \"job_id\": \"00000000-0000-0000-0000-000000000999\",
                     \"title\": \"Example Show\",
                     \"kind\": \"series\",
                     \"provider\": \"rezka\",
                     \"season\": 1,
                     \"translation\": \"AniLibria\"
                   },
                   \"progress\": {
                     \"completed_episodes\": 7,
                     \"total_episodes\": 12,
                     \"current_episode\": 8,
                     \"downloaded_bytes\": 195035136,
                     \"download_speed_bps\": 5452595,
                     \"missing_episodes\": [{\"season\": 1, \"episode\": 9}]
                   },
                   \"stage\": \"download\",
                   \"next_step\": \"process\",
                   \"actions\": [\"retry-missing\", \"resume-storage\"]
                 }'::jsonb)",
    )
    .await
    .unwrap();

    execute(
        db,
        "INSERT INTO notification_outbox
           (id, aggregate_type, aggregate_id, event_type, recipient, source_dedupe_key, payload)
         VALUES ('00000000-0000-0000-0000-000000000996', 'job',
                 '00000000-0000-0000-0000-000000000999', 'download-progress', 'primary',
                 decode(repeat('03', 32), 'hex'),
                 '{
                   \"event_type\": \"media.notification\",
                   \"schema_version\": 2,
                   \"delivery_kind\": \"card\",
                   \"card_key\": \"media-job:00000000-0000-0000-0000-000000000999\",
                   \"revision\": 8,
                   \"lifecycle_cycle\": 1,
                   \"terminal\": false,
                   \"state\": \"downloading\",
                   \"media\": {
                     \"job_id\": \"00000000-0000-0000-0000-000000000999\",
                     \"title\": \"Example Show\",
                     \"kind\": \"series\",
                     \"provider\": \"rezka\",
                     \"season\": 1,
                     \"translation\": \"AniLibria\"
                   },
                   \"progress\": {
                     \"completed_episodes\": 0,
                     \"total_episodes\": 1,
                     \"current_episode\": 13
                   },
                   \"stage\": \"download\",
                   \"next_step\": \"process\",
                   \"actions\": [\"cancel\", \"details\"]
                 }'::jsonb)",
    )
    .await
    .expect("an absolute episode number may exceed the number of tasks in the job");

    execute(
        db,
        "UPDATE notification_outbox
         SET payload = jsonb_set(
             payload,
             '{media,poster_url}',
             '\"https://image.tmdb.org/t/p/w780/example.jpg\"'::jsonb
         )
         WHERE id = '00000000-0000-0000-0000-000000000997'",
    )
    .await
    .expect("schema-v2 media notifications accept an HTTPS poster");
    assert_rejected(
        db,
        "UPDATE notification_outbox
         SET payload = jsonb_set(payload, '{media,poster_url}', '\"http://example.test/poster.jpg\"'::jsonb)
         WHERE id = '00000000-0000-0000-0000-000000000997'",
        "notification_payload_check",
    )
    .await;

    assert_rejected(
        db,
        "UPDATE notification_outbox
         SET payload = jsonb_set(payload, '{actions}', '[\"unknown\"]'::jsonb)
         WHERE id = '00000000-0000-0000-0000-000000000997'",
        "notification_payload_check",
    )
    .await;
    assert_rejected(
        db,
        "UPDATE notification_outbox
         SET payload = jsonb_set(payload, '{progress}', '{\"percentage\": 101}'::jsonb)
         WHERE id = '00000000-0000-0000-0000-000000000997'",
        "notification_payload_check",
    )
    .await;
    assert_rejected(
        db,
        "UPDATE notification_outbox
         SET payload = jsonb_set(payload, '{revision}', '9223372036854775808'::jsonb)
         WHERE id = '00000000-0000-0000-0000-000000000997'",
        "notification_payload_check",
    )
    .await;
    assert_rejected(
        db,
        "UPDATE notification_outbox
         SET payload = jsonb_set(payload, '{lifecycle_cycle}', '9223372036854775808'::jsonb)
         WHERE id = '00000000-0000-0000-0000-000000000997'",
        "notification_payload_check",
    )
    .await;
    execute(
        db,
        "UPDATE notification_outbox
         SET payload = jsonb_set(
             payload,
             '{progress}',
             '{\"missing_episodes\": [{\"season\": 0, \"episode\": 9}]}'::jsonb
         )
         WHERE id = '00000000-0000-0000-0000-000000000997'",
    )
    .await
    .expect("specials may use season zero in notification progress");
    execute(
        db,
        "UPDATE notification_outbox
         SET payload = jsonb_set(payload, '{media,season}', '0'::jsonb)
         WHERE id = '00000000-0000-0000-0000-000000000997'",
    )
    .await
    .expect("specials may use season zero in notification media");
    assert_rejected(
        db,
        "UPDATE notification_outbox
         SET payload = jsonb_set(payload, '{media,season}', '-1'::jsonb)
         WHERE id = '00000000-0000-0000-0000-000000000997'",
        "notification_payload_check",
    )
    .await;
    assert_rejected(
        db,
        "UPDATE notification_outbox
         SET payload = jsonb_set(payload, '{event_type}', 'null'::jsonb)
         WHERE id = '00000000-0000-0000-0000-000000000997'",
        "notification_payload_check",
    )
    .await;
    assert_rejected(
        db,
        "UPDATE notification_outbox
         SET payload = jsonb_set(payload, '{state}', 'null'::jsonb)
         WHERE id = '00000000-0000-0000-0000-000000000997'",
        "notification_payload_check",
    )
    .await;
    assert_rejected(
        db,
        "UPDATE notification_outbox
         SET payload = jsonb_set(payload, '{stage}', 'null'::jsonb)
         WHERE id = '00000000-0000-0000-0000-000000000997'",
        "notification_payload_check",
    )
    .await;
    assert_rejected(
        db,
        "UPDATE notification_outbox
         SET payload = jsonb_set(payload, '{next_step}', 'null'::jsonb)
         WHERE id = '00000000-0000-0000-0000-000000000997'",
        "notification_payload_check",
    )
    .await;

    assert_rejected(
        db,
        "UPDATE jobs SET notification_cycle = 0
         WHERE id = '00000000-0000-0000-0000-000000000999'",
        "jobs_notification_cycle_positive",
    )
    .await;
    assert_rejected(
        db,
        "INSERT INTO notification_outbox
           (id, aggregate_type, aggregate_id, event_type, recipient, source_dedupe_key, payload)
         VALUES ('00000000-0000-0000-0000-000000000996', 'job',
                 '00000000-0000-0000-0000-000000000999', 'started', 'primary',
                 decode(repeat('03', 32), 'hex'), '{}'::jsonb)",
        "notification_payload_check",
    )
    .await;

    // Keep the historical assertion at the structured-notification boundary
    // by rolling back through every later migration, including this one.
    Migrator::down(db, Some(20)).await.unwrap();

    let retained_rows = query(
        db,
        "SELECT id::text AS id FROM notification_outbox ORDER BY id",
    )
    .await;
    assert_eq!(
        retained_rows
            .iter()
            .map(|row| row.try_get::<String>("", "id").unwrap())
            .collect::<Vec<_>>(),
        vec!["00000000-0000-0000-0000-000000000998"],
        "down migration must delete only schema-v2 rows",
    );
    assert!(
        query(
            db,
            "SELECT column_name FROM information_schema.columns
             WHERE table_schema = 'public' AND table_name = 'jobs'
               AND column_name = 'notification_cycle'",
        )
        .await
        .is_empty(),
        "down migration must remove notification_cycle",
    );

    Migrator::up(db, Some(1)).await.unwrap();
    assert_eq!(
        query(
            db,
            "SELECT notification_cycle FROM jobs WHERE id = '00000000-0000-0000-0000-000000000999'",
        )
        .await[0]
            .try_get::<i64>("", "notification_cycle")
            .unwrap(),
        1,
        "up migration must restore notification_cycle with its default",
    );
}

#[tokio::test]
async fn detailed_notifications_migration_preserves_legacy_payloads_and_validates_new_content() {
    let test_db = TestDatabase::start().await;
    let db = test_db.connection();
    Migrator::up(db, Some(26)).await.unwrap();

    execute(
        db,
        "INSERT INTO jobs (id, owner_id, provider, result_ref, state, notify_scope)
         VALUES ('00000000-0000-0000-0000-000000000999',
                 '00000000-0000-0000-0000-000000000001', 'rezka', 'example', 'queued', 'initiator');
         INSERT INTO notification_outbox
           (id, aggregate_type, aggregate_id, event_type, recipient, source_dedupe_key, payload)
         VALUES ('00000000-0000-0000-0000-000000000994', 'job',
                 '00000000-0000-0000-0000-000000000999', 'started', 'primary',
                 decode(repeat('01', 32), 'hex'),
                 '{
                   \"event_type\": \"media.notification\",
                   \"schema_version\": 2,
                   \"delivery_kind\": \"card\",
                   \"card_key\": \"media-job:00000000-0000-0000-0000-000000000999\",
                   \"revision\": 1,
                   \"lifecycle_cycle\": 1,
                   \"terminal\": false,
                   \"state\": \"queued\",
                   \"media\": {
                     \"job_id\": \"00000000-0000-0000-0000-000000000999\",
                     \"title\": \"Example Show\",
                     \"kind\": \"series\",
                     \"provider\": \"rezka\"
                   },
                   \"actions\": [\"details\"]
                 }'::jsonb)",
    )
    .await
    .unwrap();

    Migrator::up(db, None).await.unwrap();

    execute(
        db,
        "INSERT INTO notification_outbox
           (id, aggregate_type, aggregate_id, event_type, recipient, source_dedupe_key, payload)
         VALUES
           ('00000000-0000-0000-0000-000000000993', 'job',
            '00000000-0000-0000-0000-000000000999', 'completed', 'primary',
            decode(repeat('02', 32), 'hex'),
            '{
              \"event_type\": \"media.notification\",
              \"schema_version\": 2,
              \"delivery_kind\": \"card\",
              \"card_key\": \"media-job:00000000-0000-0000-0000-000000000999\",
              \"revision\": 2,
              \"lifecycle_cycle\": 1,
              \"terminal\": true,
              \"state\": \"completed\",
              \"media\": {
                \"job_id\": \"00000000-0000-0000-0000-000000000999\",
                \"title\": \"Example Show\",
                \"kind\": \"series\",
                \"provider\": \"rezka\",
                \"origin\": \"tracked-episode\"
              },
              \"progress\": {\"completed_episodes\": 1, \"total_episodes\": 1, \"current_episode\": 8},
              \"result\": {
                \"video\": {\"codec\": \"hevc\", \"profile\": \"Main\", \"width\": 1920, \"height\": 1080},
                \"audio\": {\"language\": \"rus\", \"codec\": \"aac\", \"channels\": 2, \"channel_layout\": \"stereo\", \"title\": \"AniLibria\"},
                \"subtitles\": {\"downloaded\": 2, \"missing\": 0},
                \"file_size_bytes\": 440401920,
                \"duration_seconds\": 1421,
                \"processing\": {\"mode\": \"vaapi-upscale\", \"elapsed_seconds\": 252},
                \"publication\": {\"library\": \"tv-shows\", \"title\": \"Example Show\", \"season\": 2, \"episode\": 8}
              },
              \"actions\": [\"details\"]
            }'::jsonb),
           ('00000000-0000-0000-0000-000000000992', 'job',
            '00000000-0000-0000-0000-000000000999', 'failed', 'primary',
            decode(repeat('03', 32), 'hex'),
            '{
              \"event_type\": \"media.notification\",
              \"schema_version\": 2,
              \"delivery_kind\": \"card\",
              \"card_key\": \"media-job:00000000-0000-0000-0000-000000000999\",
              \"revision\": 3,
              \"lifecycle_cycle\": 1,
              \"terminal\": false,
              \"state\": \"needs-action\",
              \"media\": {\"job_id\": \"00000000-0000-0000-0000-000000000999\", \"title\": \"Example Show\", \"kind\": \"series\", \"provider\": \"rezka\"},
              \"progress\": {\"connection_attempt\": 5, \"connection_attempt_limit\": 20, \"vpn_rotation_pending\": true},
              \"actions\": [\"retry\", \"search-alternative\"]
            }'::jsonb),
           ('00000000-0000-0000-0000-000000000991', 'job',
            '00000000-0000-0000-0000-000000000999', 'blocked-storage', 'primary',
            decode(repeat('04', 32), 'hex'),
            '{
              \"event_type\": \"media.notification\",
              \"schema_version\": 2,
              \"delivery_kind\": \"card\",
              \"card_key\": \"media-job:00000000-0000-0000-0000-000000000999\",
              \"revision\": 4,
              \"lifecycle_cycle\": 1,
              \"terminal\": false,
              \"state\": \"needs-action\",
              \"media\": {\"job_id\": \"00000000-0000-0000-0000-000000000999\", \"title\": \"Example Show\", \"kind\": \"series\", \"provider\": \"rezka\"},
              \"progress\": {\"storage_available_bytes\": 100, \"storage_required_bytes\": 440401920},
              \"actions\": [\"resume-storage\"]
            }'::jsonb)",
    )
    .await
    .unwrap();

    let payloads = query(
        db,
        "SELECT id::text AS id FROM notification_outbox
         WHERE id IN (
           '00000000-0000-0000-0000-000000000994',
           '00000000-0000-0000-0000-000000000993',
           '00000000-0000-0000-0000-000000000992',
           '00000000-0000-0000-0000-000000000991'
         ) ORDER BY id",
    )
    .await;
    assert_eq!(
        payloads.len(),
        4,
        "all schema-v2 payload variants must survive"
    );

    assert_rejected(
        db,
        "UPDATE notification_outbox
         SET payload = jsonb_set(payload, '{result,video,width}', '0'::jsonb)
         WHERE id = '00000000-0000-0000-0000-000000000993'",
        "notification_payload_check",
    )
    .await;
    assert_rejected(
        db,
        "UPDATE notification_outbox
         SET payload = jsonb_set(payload, '{progress,connection_attempt}', '21'::jsonb)
         WHERE id = '00000000-0000-0000-0000-000000000992'",
        "notification_payload_check",
    )
    .await;
    assert_rejected(
        db,
        "UPDATE notification_outbox
         SET payload = payload #- '{progress,storage_required_bytes}'
         WHERE id = '00000000-0000-0000-0000-000000000991'",
        "notification_payload_check",
    )
    .await;
    assert_rejected(
        db,
        "UPDATE notification_outbox
         SET payload = jsonb_set(payload, '{result,unknown}', '\"value\"'::jsonb)
         WHERE id = '00000000-0000-0000-0000-000000000993'",
        "notification_payload_check",
    )
    .await;

    execute(
        db,
        r#"UPDATE notification_outbox
           SET payload = jsonb_set(
               jsonb_set(
                   jsonb_set(
                       jsonb_set(
                           jsonb_set(
                               jsonb_set(
                                   jsonb_set(
                                       jsonb_set(
                                           jsonb_set(
                                               jsonb_set(payload, '{result,video,codec}', '"H.265 / HEVC"'::jsonb),
                                               '{result,video,profile}', '"Main 10-bit"'::jsonb
                                           ),
                                           '{result,audio,language}', '"Русский / 日本語"'::jsonb
                                       ),
                                       '{result,audio,codec}', '"AAC-LC"'::jsonb
                                   ),
                                   '{result,audio,channel_layout}', '"5.1 (side)"'::jsonb
                               ),
                               '{result,audio,title}', '"AniLibria, Dub!"'::jsonb
                           ),
                           '{result,publication,title}', '"Клинки Хранителей: сезон 2"'::jsonb
                       ),
                       '{result,publication,season}', '2'::jsonb
                   ),
                   '{media,title}', '"Title / Alternate: сезон 2"'::jsonb
               ),
               '{media,translation}', '"Русский / 日本語"'::jsonb
           )
         WHERE id = '00000000-0000-0000-0000-000000000993'"#,
    )
    .await
    .expect("human-readable multilingual detailed fields must remain valid");

    for (path, value) in [
        ("{result,video,codec}", r#""https://example.invalid/video""#),
        ("{result,video,profile}", r#""/srv/media/private.mkv""#),
        ("{result,audio,language}", r#""~/private/audio""#),
        ("{result,audio,codec}", r#""C:\\media\\private.mkv""#),
        (
            "{result,audio,channel_layout}",
            r#""curl --data token=value""#,
        ),
        ("{result,audio,title}", r#""api_key=very-secret-value""#),
        ("{result,publication,title}", r#""MEDIA_PROCESSING_FAILED""#),
    ] {
        assert_rejected(
            db,
            &format!(
                "UPDATE notification_outbox\n                 SET payload = jsonb_set(payload, '{path}', $value${value}$value$::jsonb)\n                 WHERE id = '00000000-0000-0000-0000-000000000993'"
            ),
            "notification_payload_check",
        )
        .await;
    }

    for (path, value) in [
        ("{media,title}", r#""../private.mkv""#),
        ("{media,translation}", r#""media/private.mkv""#),
        ("{result,video,codec}", r#""../private.mkv""#),
        ("{result,video,profile}", r#""media/private.mkv""#),
        ("{result,audio,language}", r#""media\\private.srt""#),
        ("{result,audio,codec}", r#""ls -la""#),
        ("{result,audio,channel_layout}", r#""cat private.mkv""#),
        ("{result,audio,title}", r#""title; cat private.mkv""#),
        ("{result,publication,title}", r#""--version""#),
    ] {
        assert_rejected(
            db,
            &format!(
                "UPDATE notification_outbox\n                 SET payload = jsonb_set(payload, '{path}', $value${value}$value$::jsonb)\n                 WHERE id = '00000000-0000-0000-0000-000000000993'"
            ),
            "notification_payload_check",
        )
        .await;
    }

    assert_rejected(
        db,
        "UPDATE notification_outbox
         SET payload = jsonb_set(payload, '{media,provider}', '\"prowlarr\"'::jsonb)
         WHERE id = '00000000-0000-0000-0000-000000000993'",
        "notification_payload_check",
    )
    .await;

    // Include the detailed-notification migration in the rollback through
    // every later migration, including release-identity uniqueness.
    Migrator::down(db, Some(17)).await.unwrap();

    let normalized = query(
        db,
        "SELECT payload FROM notification_outbox
         WHERE id IN (
           '00000000-0000-0000-0000-000000000993',
           '00000000-0000-0000-0000-000000000992',
           '00000000-0000-0000-0000-000000000991'
         )",
    )
    .await;
    for row in normalized {
        let payload = row.try_get::<serde_json::Value>("", "payload").unwrap();
        assert!(payload.get("result").is_none());
        assert!(payload["media"].get("origin").is_none());
        for key in [
            "connection_attempt",
            "connection_attempt_limit",
            "vpn_rotation_pending",
            "storage_available_bytes",
            "storage_required_bytes",
        ] {
            assert!(payload["progress"].get(key).is_none());
        }
    }
}

#[tokio::test]
async fn source_choice_migration_converts_pending_legacy_tracking_notifications() {
    let test_db = TestDatabase::start().await;
    let db = test_db.connection();
    Migrator::up(db, Some(27)).await.unwrap();

    execute(
        db,
        "INSERT INTO tracking_subscriptions
           (id, owner_id, provider, title, translation, known_episodes, scope, created_operation_key)
         VALUES
           ('00000000-0000-0000-0000-000000000555',
            '00000000-0000-0000-0000-000000000001',
            'rezka', 'Jobless Reincarnation', 'release-calendar',
            '[{\"season\":3,\"episode\":5}]'::jsonb, 'personal',
            decode(repeat('55', 32), 'hex'));
         INSERT INTO tracking_discoveries (id, tracking_id, season, episode)
         VALUES
           ('00000000-0000-0000-0000-000000000556',
            '00000000-0000-0000-0000-000000000555', 3, 5);
         INSERT INTO notification_outbox
           (id, aggregate_type, aggregate_id, event_type, recipient, source_dedupe_key, payload)
         VALUES
           ('00000000-0000-0000-0000-000000000557', 'tracking',
            '00000000-0000-0000-0000-000000000555', 'future-episode-found', 'primary',
            uuid_send('00000000-0000-0000-0000-000000000556'::uuid),
            '{\"message\":\"legacy future episode\"}'::jsonb)",
    )
    .await
    .unwrap();

    Migrator::up(db, Some(1)).await.unwrap();

    let payload = query(
        db,
        "SELECT payload FROM notification_outbox
         WHERE id = '00000000-0000-0000-0000-000000000557'",
    )
    .await
    .pop()
    .unwrap()
    .try_get::<serde_json::Value>("", "payload")
    .unwrap();
    assert_eq!(payload["event_type"], "media.source-choice");
    assert_eq!(payload["schema_version"], 1);
    assert_eq!(
        payload["tracking_id"],
        "00000000-0000-0000-0000-000000000555"
    );
    assert_eq!(payload["title"], "Jobless Reincarnation");
    assert_eq!(payload["season"], 3);
    assert_eq!(payload["episode"], 5);
    assert_eq!(
        payload["actions"],
        serde_json::json!(["all", "rezka", "prowlarr"])
    );

    assert_rejected(
        db,
        "UPDATE notification_outbox
         SET payload = jsonb_set(
             payload,
             '{choice_set_id}',
             '\"00000000-0000-0000-0000-000000000777\"'::jsonb
         )
         WHERE id = '00000000-0000-0000-0000-000000000557'",
        "notification_payload_check",
    )
    .await;

    assert_rejected(
        db,
        "UPDATE notification_outbox
         SET payload = jsonb_set(payload, '{actions}', '[\"rezka\"]'::jsonb)
         WHERE id = '00000000-0000-0000-0000-000000000557'",
        "notification_payload_check",
    )
    .await;

    Migrator::down(db, Some(1)).await.unwrap();
    let legacy = query(
        db,
        "SELECT payload FROM notification_outbox
         WHERE id = '00000000-0000-0000-0000-000000000557'",
    )
    .await
    .pop()
    .unwrap()
    .try_get::<serde_json::Value>("", "payload")
    .unwrap();
    assert!(legacy.get("message").is_some());
    assert!(legacy.get("schema_version").is_none());
}

#[tokio::test]
async fn availability_gate_requeues_unverified_discoveries_and_accepts_exact_source_actions() {
    let test_db = TestDatabase::start().await;
    let db = test_db.connection();
    Migrator::up(db, Some(28)).await.unwrap();

    execute(
        db,
        "INSERT INTO tracking_subscriptions
           (id, owner_id, provider, title, translation, known_episodes, scope, created_operation_key)
         VALUES
           ('00000000-0000-0000-0000-000000000565',
            '00000000-0000-0000-0000-000000000001',
            'rezka', 'Jobless Reincarnation', 'release-calendar',
            '[{\"season\":3,\"episode\":4},{\"season\":3,\"episode\":5}]'::jsonb,
            'personal', decode(repeat('56', 32), 'hex'));
         INSERT INTO tracking_discoveries (id, tracking_id, season, episode)
         VALUES
           ('00000000-0000-0000-0000-000000000566',
            '00000000-0000-0000-0000-000000000565', 3, 5);
         INSERT INTO notification_outbox
           (id, aggregate_type, aggregate_id, event_type, recipient, source_dedupe_key, payload)
         VALUES
           ('00000000-0000-0000-0000-000000000567', 'tracking',
            '00000000-0000-0000-0000-000000000565', 'future-episode-found', 'primary',
            uuid_send('00000000-0000-0000-0000-000000000566'::uuid),
            '{
              \"event_type\":\"media.source-choice\",
              \"schema_version\":1,
              \"card_key\":\"tracking:00000000-0000-0000-0000-000000000565:3:5\",
              \"tracking_id\":\"00000000-0000-0000-0000-000000000565\",
              \"title\":\"Jobless Reincarnation\",
              \"season\":3,
              \"episode\":5,
              \"actions\":[\"all\",\"rezka\",\"prowlarr\"]
            }'::jsonb)",
    )
    .await
    .unwrap();

    Migrator::up(db, Some(1)).await.unwrap();

    let tracking = query(
        db,
        "SELECT known_episodes, next_check_at <= now() AS due
         FROM tracking_subscriptions
         WHERE id = '00000000-0000-0000-0000-000000000565'",
    )
    .await
    .pop()
    .unwrap();
    assert_eq!(
        tracking
            .try_get::<serde_json::Value>("", "known_episodes")
            .unwrap(),
        serde_json::json!([{"season": 3, "episode": 4}])
    );
    assert!(tracking.try_get::<bool>("", "due").unwrap());
    assert!(
        query(
            db,
            "SELECT id FROM tracking_discoveries
             WHERE id = '00000000-0000-0000-0000-000000000566'",
        )
        .await
        .is_empty()
    );
    let outbox = query(
        db,
        "SELECT dead_at IS NOT NULL AS dead, last_error_code
         FROM notification_outbox
         WHERE id = '00000000-0000-0000-0000-000000000567'",
    )
    .await
    .pop()
    .unwrap();
    assert!(outbox.try_get::<bool>("", "dead").unwrap());
    assert_eq!(
        outbox.try_get::<String>("", "last_error_code").unwrap(),
        "availability_unverified"
    );

    execute(
        db,
        "UPDATE notification_outbox
         SET payload = jsonb_set(payload, '{actions}', '[\"rezka\"]'::jsonb)
         WHERE id = '00000000-0000-0000-0000-000000000567'",
    )
    .await
    .expect("a single confirmed source must satisfy the payload constraint");

    Migrator::down(db, Some(1)).await.unwrap();

    let restored = query(
        db,
        "SELECT payload, dead_at, last_error_code
         FROM notification_outbox
         WHERE id = '00000000-0000-0000-0000-000000000567'",
    )
    .await
    .pop()
    .unwrap();
    assert_eq!(
        restored
            .try_get::<serde_json::Value>("", "payload")
            .unwrap()["actions"],
        serde_json::json!(["all", "rezka", "prowlarr"])
    );
    assert!(
        restored
            .try_get::<Option<time::OffsetDateTime>>("", "dead_at")
            .unwrap()
            .is_none()
    );
    assert!(
        restored
            .try_get::<Option<String>>("", "last_error_code")
            .unwrap()
            .is_none()
    );
    assert_eq!(
        query(
            db,
            "SELECT id FROM tracking_discoveries
             WHERE id = '00000000-0000-0000-0000-000000000566'",
        )
        .await
        .len(),
        1
    );
}

#[tokio::test]
async fn recent_delivered_calendar_discovery_is_rechecked_without_touching_older_history() {
    let test_db = TestDatabase::start().await;
    let db = test_db.connection();
    Migrator::up(db, Some(28)).await.unwrap();

    execute(
        db,
        "INSERT INTO tracking_subscriptions
           (id, owner_id, provider, title, translation, known_episodes, scope, created_operation_key)
         VALUES
           ('00000000-0000-0000-0000-000000000575',
            '00000000-0000-0000-0000-000000000001',
            'rezka', 'Future Show', 'release-calendar',
            '[{\"season\":3,\"episode\":4},{\"season\":3,\"episode\":5}]'::jsonb,
            'personal', decode(repeat('57', 32), 'hex'));
         INSERT INTO tracking_discoveries (id, tracking_id, season, episode)
         VALUES
           ('00000000-0000-0000-0000-000000000576',
            '00000000-0000-0000-0000-000000000575', 3, 5);
         INSERT INTO notification_outbox
           (id, aggregate_type, aggregate_id, event_type, recipient, source_dedupe_key,
            payload, delivered_at)
         VALUES
           ('00000000-0000-0000-0000-000000000577', 'tracking',
            '00000000-0000-0000-0000-000000000575', 'future-episode-found', 'primary',
            uuid_send('00000000-0000-0000-0000-000000000576'::uuid),
            '{\"message\":\"calendar-only notification\"}'::jsonb, now())",
    )
    .await
    .unwrap();

    Migrator::up(db, Some(2)).await.unwrap();

    let tracking = query(
        db,
        "SELECT known_episodes, next_check_at <= now() AS due
         FROM tracking_subscriptions
         WHERE id = '00000000-0000-0000-0000-000000000575'",
    )
    .await
    .pop()
    .unwrap();
    assert_eq!(
        tracking
            .try_get::<serde_json::Value>("", "known_episodes")
            .unwrap(),
        serde_json::json!([{"season": 3, "episode": 4}])
    );
    assert!(tracking.try_get::<bool>("", "due").unwrap());
    assert!(
        query(
            db,
            "SELECT id FROM tracking_discoveries
             WHERE id = '00000000-0000-0000-0000-000000000576'",
        )
        .await
        .is_empty()
    );
    assert_eq!(
        query(
            db,
            "SELECT last_error_code FROM notification_outbox
             WHERE id = '00000000-0000-0000-0000-000000000577'",
        )
        .await
        .pop()
        .unwrap()
        .try_get::<String>("", "last_error_code")
        .unwrap(),
        "availability_unverified:3:5"
    );

    Migrator::down(db, Some(1)).await.unwrap();

    assert_eq!(
        query(
            db,
            "SELECT id FROM tracking_discoveries
             WHERE id = '00000000-0000-0000-0000-000000000576'",
        )
        .await
        .len(),
        1
    );
    let restored = query(
        db,
        "SELECT known_episodes FROM tracking_subscriptions
         WHERE id = '00000000-0000-0000-0000-000000000575'",
    )
    .await
    .pop()
    .unwrap()
    .try_get::<serde_json::Value>("", "known_episodes")
    .unwrap();
    assert_eq!(
        restored,
        serde_json::json!([
            {"season": 3, "episode": 4},
            {"season": 3, "episode": 5}
        ])
    );
}

#[tokio::test]
async fn older_season_backfill_is_removed_without_touching_existing_history() {
    let test_db = TestDatabase::start().await;
    let db = test_db.connection();
    Migrator::up(db, Some(30)).await.unwrap();

    execute(
        db,
        "INSERT INTO tracking_subscriptions
           (id, owner_id, provider, title, translation, known_episodes, scope, created_operation_key)
         VALUES
           ('00000000-0000-0000-0000-000000000585',
            '00000000-0000-0000-0000-000000000001',
            'rezka', 'Long-running Show', 'release-calendar',
            '[
              {\"season\":1,\"episode\":1},
              {\"season\":1,\"episode\":2},
              {\"season\":9,\"episode\":9}
            ]'::jsonb,
            'personal', decode(repeat('58', 32), 'hex'));
         INSERT INTO tracking_discoveries (id, tracking_id, season, episode)
         VALUES
           ('00000000-0000-0000-0000-000000000586',
            '00000000-0000-0000-0000-000000000585', 1, 2);
         INSERT INTO notification_outbox
           (id, aggregate_type, aggregate_id, event_type, recipient, source_dedupe_key,
            payload, delivered_at)
         VALUES
           ('00000000-0000-0000-0000-000000000587', 'tracking',
            '00000000-0000-0000-0000-000000000585', 'future-episode-found', 'primary',
            uuid_send('00000000-0000-0000-0000-000000000586'::uuid),
            '{
              \"event_type\":\"media.source-choice\",
              \"schema_version\":1,
              \"card_key\":\"tracking:00000000-0000-0000-0000-000000000585:1:2\",
              \"tracking_id\":\"00000000-0000-0000-0000-000000000585\",
              \"title\":\"Long-running Show\",
              \"season\":1,
              \"episode\":2,
              \"actions\":[\"rezka\"]
            }'::jsonb, now())",
    )
    .await
    .unwrap();

    Migrator::up(db, Some(1)).await.unwrap();

    let known = query(
        db,
        "SELECT known_episodes FROM tracking_subscriptions
         WHERE id = '00000000-0000-0000-0000-000000000585'",
    )
    .await
    .pop()
    .unwrap()
    .try_get::<serde_json::Value>("", "known_episodes")
    .unwrap();
    assert_eq!(
        known,
        serde_json::json!([
            {"season": 1, "episode": 1},
            {"season": 9, "episode": 9}
        ])
    );
    assert!(
        query(
            db,
            "SELECT id FROM tracking_discoveries
             WHERE id = '00000000-0000-0000-0000-000000000586'",
        )
        .await
        .is_empty()
    );
    assert_eq!(
        query(
            db,
            "SELECT last_error_code FROM notification_outbox
             WHERE id = '00000000-0000-0000-0000-000000000587'",
        )
        .await
        .pop()
        .unwrap()
        .try_get::<String>("", "last_error_code")
        .unwrap(),
        "superseded_older_season:1:2"
    );

    Migrator::down(db, Some(1)).await.unwrap();
    let restored = query(
        db,
        "SELECT known_episodes FROM tracking_subscriptions
         WHERE id = '00000000-0000-0000-0000-000000000585'",
    )
    .await
    .pop()
    .unwrap()
    .try_get::<serde_json::Value>("", "known_episodes")
    .unwrap();
    assert_eq!(
        restored,
        serde_json::json!([
            {"season": 1, "episode": 1},
            {"season": 9, "episode": 9},
            {"season": 1, "episode": 2}
        ])
    );
}

#[tokio::test]
async fn inaccurate_prowlarr_matches_are_removed_and_only_future_candidates_are_retained() {
    let test_db = TestDatabase::start().await;
    let db = test_db.connection();
    Migrator::up(db, Some(31)).await.unwrap();

    execute(
        db,
        "INSERT INTO tracking_subscriptions
           (id, owner_id, provider, title, translation, known_episodes, scope,
            created_operation_key)
         VALUES
           ('00000000-0000-0000-0000-000000000595',
            '00000000-0000-0000-0000-000000000001',
            'rezka', 'Long-running Show', 'release-calendar',
            '[
              {\"season\":9,\"episode\":8},
              {\"season\":9,\"episode\":9},
              {\"season\":9,\"episode\":1},
              {\"season\":9,\"episode\":4},
              {\"season\":9,\"episode\":10}
            ]'::jsonb,
            'personal', decode(repeat('59', 32), 'hex'));
         INSERT INTO tracking_discoveries (id, tracking_id, season, episode)
         VALUES
           ('00000000-0000-0000-0000-000000000596',
            '00000000-0000-0000-0000-000000000595', 9, 1),
           ('00000000-0000-0000-0000-000000000597',
            '00000000-0000-0000-0000-000000000595', 9, 4),
           ('00000000-0000-0000-0000-000000000598',
            '00000000-0000-0000-0000-000000000595', 9, 10);
         INSERT INTO notification_outbox
           (id, aggregate_type, aggregate_id, event_type, recipient,
            source_dedupe_key, payload, delivered_at)
         VALUES
           ('00000000-0000-0000-0000-000000000591', 'tracking',
            '00000000-0000-0000-0000-000000000595', 'future-episode-found',
            'primary', uuid_send('00000000-0000-0000-0000-000000000596'::uuid),
            jsonb_build_object(
              'event_type', 'media.source-choice',
              'schema_version', 1,
              'card_key',
                'tracking:00000000-0000-0000-0000-000000000595:9:1',
              'tracking_id', '00000000-0000-0000-0000-000000000595',
              'title', 'Long-running Show',
              'season', 9,
              'episode', 1,
              'actions', jsonb_build_array('prowlarr')
            ), now()),
           ('00000000-0000-0000-0000-000000000592', 'tracking',
            '00000000-0000-0000-0000-000000000595', 'future-episode-found',
            'primary', uuid_send('00000000-0000-0000-0000-000000000597'::uuid),
            jsonb_build_object(
              'event_type', 'media.source-choice',
              'schema_version', 1,
              'card_key',
                'tracking:00000000-0000-0000-0000-000000000595:9:4',
              'tracking_id', '00000000-0000-0000-0000-000000000595',
              'title', 'Long-running Show',
              'season', 9,
              'episode', 4,
              'actions', jsonb_build_array('prowlarr')
            ), now()),
           ('00000000-0000-0000-0000-000000000593', 'tracking',
            '00000000-0000-0000-0000-000000000595', 'future-episode-found',
            'primary', uuid_send('00000000-0000-0000-0000-000000000598'::uuid),
            jsonb_build_object(
              'event_type', 'media.source-choice',
              'schema_version', 1,
              'card_key',
                'tracking:00000000-0000-0000-0000-000000000595:9:10',
              'tracking_id', '00000000-0000-0000-0000-000000000595',
              'title', 'Long-running Show',
              'season', 9,
              'episode', 10,
              'actions', jsonb_build_array('prowlarr')
            ), now())",
    )
    .await
    .unwrap();

    Migrator::up(db, Some(1)).await.unwrap();

    let known = query(
        db,
        "SELECT known_episodes FROM tracking_subscriptions
         WHERE id = '00000000-0000-0000-0000-000000000595'",
    )
    .await
    .pop()
    .unwrap()
    .try_get::<serde_json::Value>("", "known_episodes")
    .unwrap();
    assert_eq!(
        known,
        serde_json::json!([
            {"season": 9, "episode": 8},
            {"season": 9, "episode": 9}
        ])
    );

    let candidates = query(
        db,
        "SELECT season, episode FROM tracking_availability_candidates
         WHERE tracking_id = '00000000-0000-0000-0000-000000000595'
         ORDER BY season, episode",
    )
    .await;
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].try_get::<i32>("", "season").unwrap(), 9);
    assert_eq!(candidates[0].try_get::<i32>("", "episode").unwrap(), 10);
    assert!(
        query(
            db,
            "SELECT id FROM tracking_discoveries
             WHERE tracking_id = '00000000-0000-0000-0000-000000000595'",
        )
        .await
        .is_empty()
    );
    assert_eq!(
        query(
            db,
            "SELECT id FROM notification_outbox
             WHERE aggregate_id = '00000000-0000-0000-0000-000000000595'
               AND last_error_code ~ '^prowlarr_coordinate_unverified:'",
        )
        .await
        .len(),
        3
    );

    Migrator::down(db, Some(1)).await.unwrap();

    assert!(
        query(
            db,
            "SELECT table_name FROM information_schema.tables
             WHERE table_schema = 'public'
               AND table_name = 'tracking_availability_candidates'",
        )
        .await
        .is_empty()
    );
    assert_eq!(
        query(
            db,
            "SELECT id FROM tracking_discoveries
             WHERE tracking_id = '00000000-0000-0000-0000-000000000595'",
        )
        .await
        .len(),
        3
    );
}

#[tokio::test]
async fn source_choice_posters_accept_only_bounded_https_urls_and_reverse_cleanly() {
    let test_db = TestDatabase::start().await;
    let db = test_db.connection();
    Migrator::up(db, Some(36)).await.unwrap();

    execute(
        db,
        "INSERT INTO notification_outbox
           (id, aggregate_type, aggregate_id, event_type, recipient, source_dedupe_key, payload)
         VALUES
           ('00000000-0000-0000-0000-000000000637', 'tracking',
            '00000000-0000-0000-0000-000000000635', 'future-episode-found', 'primary',
            decode(repeat('63', 32), 'hex'),
            '{
              \"event_type\":\"media.source-choice\",
              \"schema_version\":1,
              \"card_key\":\"tracking:00000000-0000-0000-0000-000000000635:3:7\",
              \"tracking_id\":\"00000000-0000-0000-0000-000000000635\",
              \"title\":\"Jobless Reincarnation\",
              \"season\":3,
              \"episode\":7,
              \"actions\":[\"rezka\"]
            }'::jsonb)",
    )
    .await
    .unwrap();

    // Apply both the poster and choice-set migrations, then verify that
    // rolling back the latter restores the poster-aware validator.
    Migrator::up(db, Some(2)).await.unwrap();

    execute(
        db,
        "UPDATE notification_outbox
         SET payload = jsonb_set(
             payload,
             '{poster_url}',
             '\"https://static.tvmaze.com/uploads/images/original_untouched/1/2.jpg\"'::jsonb
         )
         WHERE id = '00000000-0000-0000-0000-000000000637'",
    )
    .await
    .expect("source-choice payloads accept an HTTPS poster");

    for value in [
        r#""""#,
        r#""http://static.tvmaze.com/poster.jpg""#,
        r#""https://user@example.test/poster.jpg""#,
        "null",
    ] {
        assert_rejected(
            db,
            &format!(
                "UPDATE notification_outbox
                 SET payload = jsonb_set(payload, '{{poster_url}}', $value${value}$value$::jsonb)
                 WHERE id = '00000000-0000-0000-0000-000000000637'"
            ),
            "notification_payload_check",
        )
        .await;
    }

    assert_rejected(
        db,
        "UPDATE notification_outbox
         SET payload = jsonb_set(
             payload,
             '{poster_url}',
             to_jsonb('https://example.test/' || repeat('a', 2049))
         )
         WHERE id = '00000000-0000-0000-0000-000000000637'",
        "notification_payload_check",
    )
    .await;
    assert_rejected(
        db,
        "UPDATE notification_outbox
         SET payload = jsonb_set(payload, '{unexpected}', 'true'::jsonb)
         WHERE id = '00000000-0000-0000-0000-000000000637'",
        "notification_payload_check",
    )
    .await;

    Migrator::down(db, Some(1)).await.unwrap();

    let payload = query(
        db,
        "SELECT payload FROM notification_outbox
         WHERE id = '00000000-0000-0000-0000-000000000637'",
    )
    .await
    .pop()
    .unwrap()
    .try_get::<serde_json::Value>("", "payload")
    .unwrap();
    assert_eq!(
        payload["poster_url"],
        serde_json::json!("https://static.tvmaze.com/uploads/images/original_untouched/1/2.jpg")
    );
    assert_eq!(payload["actions"], serde_json::json!(["rezka"]));

    execute(
        db,
        "UPDATE notification_outbox
         SET payload = jsonb_set(
             payload,
             '{poster_url}',
             '\"https://static.tvmaze.com/poster.jpg\"'::jsonb
         )
         WHERE id = '00000000-0000-0000-0000-000000000637'",
    )
    .await
    .expect("migration 37 validator must still accept HTTPS poster URLs after rollback");
    assert_eq!(
        query(
            db,
            "SELECT payload->>'poster_url' AS poster_url
             FROM notification_outbox
             WHERE id = '00000000-0000-0000-0000-000000000637'",
        )
        .await
        .pop()
        .unwrap()
        .try_get::<String>("", "poster_url")
        .unwrap(),
        "https://static.tvmaze.com/poster.jpg"
    );

    assert_rejected(
        db,
        "UPDATE notification_outbox
         SET payload = jsonb_set(payload, '{poster_url}', '\"http://static.tvmaze.com/poster.jpg\"'::jsonb)
         WHERE id = '00000000-0000-0000-0000-000000000637'",
        "notification_payload_check",
    )
    .await;
}

#[tokio::test]
async fn source_choice_season_complete_is_optional_boolean_and_reverse_cleanly() {
    let test_db = TestDatabase::start().await;
    let db = test_db.connection();
    Migrator::up(db, Some(40)).await.unwrap();

    execute(
        db,
        "INSERT INTO notification_outbox
           (id, aggregate_type, aggregate_id, event_type, recipient, source_dedupe_key, payload)
         VALUES
           ('00000000-0000-0000-0000-000000000641', 'tracking',
            '00000000-0000-0000-0000-000000000640', 'future-episode-found', 'primary',
            decode(repeat('64', 32), 'hex'),
            '{
              \"event_type\":\"media.source-choice\",
              \"schema_version\":1,
              \"card_key\":\"tracking:00000000-0000-0000-0000-000000000640:3:12\",
              \"tracking_id\":\"00000000-0000-0000-0000-000000000640\",
              \"title\":\"Jobless Reincarnation\",
              \"season\":3,
              \"episode\":12,
              \"actions\":[\"rezka\"]
            }'::jsonb)",
    )
    .await
    .unwrap();

    Migrator::up(db, Some(1)).await.unwrap();

    execute(
        db,
        "UPDATE notification_outbox
         SET payload = jsonb_set(payload, '{season_complete}', 'true'::jsonb)
         WHERE id = '00000000-0000-0000-0000-000000000641'",
    )
    .await
    .expect("source-choice payloads accept season_complete true");
    execute(
        db,
        "UPDATE notification_outbox
         SET payload = jsonb_set(payload, '{season_complete}', 'false'::jsonb)
         WHERE id = '00000000-0000-0000-0000-000000000641'",
    )
    .await
    .expect("source-choice payloads accept season_complete false");

    assert_rejected(
        db,
        "UPDATE notification_outbox
         SET payload = jsonb_set(payload, '{season_complete}', '1'::jsonb)
         WHERE id = '00000000-0000-0000-0000-000000000641'",
        "notification_payload_check",
    )
    .await;
    assert_rejected(
        db,
        "UPDATE notification_outbox
         SET payload = jsonb_set(payload, '{unexpected}', 'true'::jsonb)
         WHERE id = '00000000-0000-0000-0000-000000000641'",
        "notification_payload_check",
    )
    .await;

    execute(
        db,
        "UPDATE notification_outbox
         SET payload = jsonb_set(payload, '{season_complete}', 'true'::jsonb)
         WHERE id = '00000000-0000-0000-0000-000000000641'",
    )
    .await
    .unwrap();

    Migrator::down(db, Some(1)).await.unwrap();

    let payload = query(
        db,
        "SELECT payload FROM notification_outbox
         WHERE id = '00000000-0000-0000-0000-000000000641'",
    )
    .await
    .pop()
    .unwrap()
    .try_get::<serde_json::Value>("", "payload")
    .unwrap();
    assert!(payload.get("season_complete").is_none());
    assert_eq!(payload["actions"], serde_json::json!(["rezka"]));
}

#[tokio::test]
async fn tracking_posters_are_constrained_and_reverse_cleanly() {
    let test_db = TestDatabase::start().await;
    let db = test_db.connection();
    Migrator::up(db, Some(38)).await.unwrap();
    execute(
        db,
        &format!(
            "INSERT INTO tracking_subscriptions
               (id, owner_id, provider, title, translation, known_episodes, scope, created_operation_key)
             VALUES
               ('00000000-0000-0000-0000-000000000639', '{PRIMARY_ID}', 'rezka',
                'Poster Show', 'release-calendar', '[{{\"season\":1,\"episode\":1}}]'::jsonb,
                'personal', decode(repeat('39', 32), 'hex'))"
        ),
    )
    .await
    .unwrap();

    Migrator::up(db, Some(1)).await.unwrap();
    execute(
        db,
        "UPDATE tracking_subscriptions
         SET poster_url = 'https://image.tmdb.org/t/p/w780/show.jpg'
         WHERE id = '00000000-0000-0000-0000-000000000639'",
    )
    .await
    .unwrap();
    for value in [
        "http://example.test/poster.jpg",
        "https://user@example.test/poster.jpg",
        "https://example.test/poster.jpg#fragment",
    ] {
        assert_rejected(
            db,
            &format!(
                "UPDATE tracking_subscriptions SET poster_url = '{value}'
                 WHERE id = '00000000-0000-0000-0000-000000000639'"
            ),
            "tracking_poster_url_check",
        )
        .await;
    }

    Migrator::down(db, Some(1)).await.unwrap();
    assert!(
        query(
            db,
            "SELECT column_name FROM information_schema.columns
             WHERE table_schema = 'public'
               AND table_name = 'tracking_subscriptions'
               AND column_name = 'poster_url'",
        )
        .await
        .is_empty()
    );
}

#[tokio::test]
async fn postgres_enforces_domain_and_concurrency_invariants() {
    let test_db = TestDatabase::start().await;
    let db = test_db.connection();
    Migrator::up(db, None).await.unwrap();

    assert_rejected(
        db,
        "INSERT INTO api_clients
           (id, name, role, user_id, credential_digest)
         VALUES
           ('00000000-0000-0001-0000-000000000001', 'ownerless-hermes',
            'hermes', NULL, decode(repeat('01', 32), 'hex'))",
        "api_clients_fixed_identity_check",
    )
    .await;

    assert_rejected(
        db,
        "INSERT INTO operation_receipts
           (id, operation_key, operation_kind, result_kind)
         VALUES ('00000000-0000-0024-0000-000000000001',
                 decode(repeat('01', 31), 'hex'), 'create_job', 'pending')",
        "operation_receipts_key_length_check",
    )
    .await;
    assert_rejected(
        db,
        "INSERT INTO operation_receipts
           (id, operation_key, operation_kind, result_kind, result_snapshot)
         VALUES ('00000000-0000-0024-0000-000000000006',
                 decode(repeat('06', 32), 'hex'), 'create_job', 'lease', '{}'::jsonb)",
        "operation_receipts_kind_result_check",
    )
    .await;
    assert_rejected(
        db,
        "INSERT INTO operation_receipts
           (id, operation_key, operation_kind, result_kind)
         VALUES ('00000000-0000-0024-0000-000000000002',
                 decode(repeat('02', 32), 'hex'), 'unknown', 'pending')",
        "operation_receipts_kind_check",
    )
    .await;
    assert_rejected(
        db,
        "INSERT INTO operation_receipts
           (id, operation_key, operation_kind, result_kind, result_snapshot)
         VALUES ('00000000-0000-0024-0000-000000000003',
                 decode(repeat('03', 32), 'hex'), 'lease_next', 'none', '{}'::jsonb)",
        "operation_receipts_result_shape_check",
    )
    .await;
    assert_rejected(
        db,
        "INSERT INTO operation_receipts
           (id, operation_key, operation_kind, result_kind, result_snapshot)
         VALUES ('00000000-0000-0024-0000-000000000007',
                 decode(repeat('07', 32), 'hex'), 'create_job', 'job', NULL)",
        "operation_receipts_result_shape_check",
    )
    .await;
    execute(
        db,
        "INSERT INTO operation_receipts
           (id, operation_key, operation_kind, result_kind)
         VALUES ('00000000-0000-0024-0000-000000000004',
                 decode(repeat('04', 32), 'hex'), 'heartbeat', 'pending')",
    )
    .await
    .unwrap();
    assert_rejected(
        db,
        "INSERT INTO operation_receipts
           (id, operation_key, operation_kind, result_kind)
         VALUES ('00000000-0000-0024-0000-000000000005',
                 decode(repeat('04', 32), 'hex'), 'heartbeat', 'pending')",
        "operation_receipts_operation_key_key",
    )
    .await;

    assert_rejected(
        db,
        "INSERT INTO api_clients
           (id, name, role, user_id, credential_digest)
         VALUES
           ('00000000-0000-0000-0001-000000000001', 'swapped-hermes',
            'hermes', '00000000-0000-0000-0000-000000000002',
            decode(repeat('04', 32), 'hex'))",
        "api_clients_fixed_identity_check",
    )
    .await;
    assert_rejected(
        db,
        "INSERT INTO api_clients
           (id, name, role, user_id, credential_digest)
         VALUES
           ('00000000-0000-0002-0000-000000000099', 'unknown-runner',
            'runner', NULL, decode(repeat('05', 32), 'hex'))",
        "api_clients_fixed_identity_check",
    )
    .await;

    assert_rejected(
        db,
        "INSERT INTO media (id, kind, title, series_ordering)
         VALUES ('00000000-0000-0010-0000-000000000001', 'series', 'Bad ordering', 'imdb')",
        "media_kind_ordering_check",
    )
    .await;
    assert_rejected(
        db,
        "INSERT INTO media (id, kind, title, series_ordering)
         VALUES ('00000000-0000-0010-0000-000000000003', 'series', 'Missing ordering', NULL)",
        "media_kind_ordering_check",
    )
    .await;

    execute(
        db,
        "INSERT INTO media (id, kind, title, series_ordering)
         VALUES ('00000000-0000-0010-0000-000000000002', 'series', 'Valid series', 'tmdb_aired');
         INSERT INTO seasons (id, media_id, season_number)
         VALUES ('00000000-0000-0011-0000-000000000001',
                 '00000000-0000-0010-0000-000000000002', 1);
         INSERT INTO episodes (id, season_id, episode_number)
         VALUES
           ('00000000-0000-0012-0000-000000000001',
            '00000000-0000-0011-0000-000000000001', 1),
           ('00000000-0000-0012-0000-000000000002',
            '00000000-0000-0011-0000-000000000001', 2);
         INSERT INTO episode_provider_mappings
           (id, episode_id, provider, provider_media_ref,
            provider_season_number, provider_episode_number, source)
         VALUES
           ('00000000-0000-0013-0000-000000000001',
            '00000000-0000-0012-0000-000000000001', 'rezka', 'rezka-title', 1, 1,
            'confirmed_by_user')",
    )
    .await
    .unwrap();

    assert_rejected(
        db,
        "INSERT INTO episode_provider_mappings
           (id, episode_id, provider, provider_media_ref,
            provider_season_number, provider_episode_number, source)
         VALUES
           ('00000000-0000-0013-0000-000000000002',
            '00000000-0000-0012-0000-000000000002', 'rezka', 'rezka-title', 1, 1,
            'discovered')",
        "episode_provider_mappings_provider_coordinate_key",
    )
    .await;

    assert_rejected(
        db,
        &format!(
            "INSERT INTO jobs
               (id, owner_id, provider, result_ref, state, needs_action_reason, notify_scope)
             VALUES
               ('00000000-0000-0020-0000-000000000001', '{PRIMARY_ID}', 'rezka',
                'selected-result', 'queued', 'identity_ambiguous', 'initiator')"
        ),
        "jobs_state_reason_check",
    )
    .await;

    assert_rejected(
        db,
        &format!(
            "INSERT INTO jobs
               (id, owner_id, provider, result_ref, state, notify_scope)
             VALUES
               ('00000000-0000-0020-0000-000000000006', '{PRIMARY_ID}', 'rezka',
                repeat('x', 65537), 'queued', 'initiator')"
        ),
        "jobs_result_ref_length_check",
    )
    .await;

    execute(
        db,
        &format!(
            "INSERT INTO api_clients
               (id, name, role, user_id, credential_digest)
             VALUES
               ('00000000-0000-0000-0002-000000000001', 'runner', 'runner', NULL,
                decode(repeat('02', 32), 'hex'));
             INSERT INTO jobs
               (id, owner_id, provider, result_ref, state, notify_scope)
             VALUES
               ('00000000-0000-0020-0000-000000000002', '{PRIMARY_ID}', 'rezka',
                'result-2', 'leased', 'initiator'),
               ('00000000-0000-0020-0000-000000000003', '{SECONDARY_ID}', 'prowlarr',
                'result-3', 'queued', 'initiator'),
               ('00000000-0000-0020-0000-000000000004', '{SECONDARY_ID}', 'rezka',
                'result-4', 'queued', 'family');
             INSERT INTO job_leases
               (id, slot, job_id, runner_client_id, expires_at)
             VALUES
               ('00000000-0000-0021-0000-000000000001', 1,
                '00000000-0000-0020-0000-000000000002',
                '00000000-0000-0000-0002-000000000001', now() + interval '1 minute')"
        ),
    )
    .await
    .unwrap();

    assert_rejected(
        db,
        "UPDATE jobs SET state = 'leased' \
         WHERE id = '00000000-0000-0020-0000-000000000003'",
        "jobs_single_active_idx",
    )
    .await;

    assert_rejected(
        db,
        "INSERT INTO job_leases
           (id, slot, job_id, runner_client_id, expires_at)
         VALUES
           ('00000000-0000-0021-0000-000000000002', 1,
            '00000000-0000-0020-0000-000000000003',
            '00000000-0000-0000-0002-000000000001', now() + interval '1 minute')",
        "job_leases_slot_key",
    )
    .await;

    assert_rejected(
        db,
        "DELETE FROM jobs
         WHERE id = '00000000-0000-0020-0000-000000000002'",
        "job_leases_job_id_fkey",
    )
    .await;
    assert_rejected(
        db,
        "INSERT INTO job_leases
           (id, slot, job_id, runner_client_id, expires_at)
         VALUES
           ('00000000-0000-0021-0000-000000000003', 2,
            '00000000-0000-0020-0000-000000000004',
            '00000000-0000-0000-0002-000000000001', now() + interval '1 minute')",
        "job_leases_slot_check",
    )
    .await;

    assert_rejected(
        db,
        "INSERT INTO job_tasks
           (id, job_id, ordinal, state, attempt_count)
         VALUES
           ('00000000-0000-0022-0000-000000000001',
            '00000000-0000-0020-0000-000000000002', -1, 'pending', 0)",
        "job_tasks_ordinal_check",
    )
    .await;
    assert_rejected(
        db,
        &format!(
            "INSERT INTO jobs
               (id, owner_id, provider, result_ref, state, notify_scope, attempt_count)
             VALUES
               ('00000000-0000-0020-0000-000000000005', '{PRIMARY_ID}', 'rezka',
                'negative-attempt', 'queued', 'initiator', -1)"
        ),
        "jobs_attempt_count_check",
    )
    .await;
    assert_rejected(
        db,
        "INSERT INTO idempotency_records
           (id, client_id, idempotency_key, request_hash, generation, status, expires_at)
         VALUES
           ('00000000-0000-0023-0000-000000000001',
            '00000000-0000-0000-0002-000000000001', 'bad-status',
            decode(repeat('03', 32), 'hex'),
            '00000000-0000-0030-0000-000000000001', 'unknown',
            now() + interval '1 day')",
        "idempotency_records_status_check",
    )
    .await;

    execute(
        db,
        "INSERT INTO idempotency_records
           (id, client_id, idempotency_key, request_hash, status,
            generation, response_status, response_content_type, response_body, expires_at)
         VALUES
           ('00000000-0000-0023-0000-000000000002',
            '00000000-0000-0000-0002-000000000001', 'empty-204',
            decode(repeat('04', 32), 'hex'), 'completed',
            '00000000-0000-0030-0000-000000000002', 204, '', ''::bytea,
            now() + interval '1 day')",
    )
    .await
    .unwrap();

    let database_backend = db.get_database_backend();
    assert_eq!(database_backend, sea_orm::DbBackend::Postgres);
}
