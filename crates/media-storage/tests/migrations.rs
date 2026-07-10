mod support;

use std::collections::{BTreeMap, BTreeSet};

use media_storage::Migrator;
use sea_orm_migration::MigratorTrait;
use support::{TestDatabase, assert_rejected, execute, query};

const PRIMARY_ID: &str = "00000000-0000-0000-0000-000000000001";
const SECONDARY_ID: &str = "00000000-0000-0000-0000-000000000002";

const APPLICATION_TABLES: [&str; 12] = [
    "api_clients",
    "episode_provider_mappings",
    "episodes",
    "idempotency_records",
    "job_leases",
    "job_stages",
    "job_tasks",
    "jobs",
    "media",
    "media_external_refs",
    "seasons",
    "users",
];

#[tokio::test]
async fn migrations_apply_seed_fixed_users_and_reverse_cleanly() {
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
        APPLICATION_TABLES.into_iter().map(str::to_owned).collect()
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
        "job_tasks.checkpoint",
        "job_tasks.error_snapshot",
        "jobs.error_snapshot",
        "jobs.request_snapshot",
        "media.metadata_snapshot",
        "media_external_refs.provider_snapshot",
        "seasons.metadata_snapshot",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<BTreeSet<_>>();
    assert_eq!(jsonb_columns, expected_jsonb);

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

    Migrator::down(db, None)
        .await
        .expect("all explicit migrations must reverse in dependency order");

    let remaining = query(
        db,
        "SELECT table_name
         FROM information_schema.tables
         WHERE table_schema = 'public'
           AND table_name = ANY (ARRAY[
             'users', 'api_clients', 'media', 'media_external_refs',
             'seasons', 'episodes', 'episode_provider_mappings', 'jobs',
             'job_tasks', 'job_stages', 'idempotency_records', 'job_leases'
           ])",
    )
    .await;
    assert!(remaining.is_empty(), "down must remove every owned table");
    assert!(
        Migrator::get_applied_migrations(db)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_failed_migration_explicitly_rolls_back_partial_schema() {
    for migration in Migrator::migrations() {
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
                'result-3', 'leased', 'initiator'),
               ('00000000-0000-0020-0000-000000000004', '{SECONDARY_ID}', 'rezka',
                'result-4', 'leased', 'family');
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
           (id, client_id, idempotency_key, request_hash, status, expires_at)
         VALUES
           ('00000000-0000-0023-0000-000000000001',
            '00000000-0000-0000-0002-000000000001', 'bad-status',
            decode(repeat('03', 32), 'hex'), 'unknown', now() + interval '1 day')",
        "idempotency_records_status_check",
    )
    .await;

    execute(
        db,
        "INSERT INTO idempotency_records
           (id, client_id, idempotency_key, request_hash, status,
            response_status, response_content_type, response_body, expires_at)
         VALUES
           ('00000000-0000-0023-0000-000000000002',
            '00000000-0000-0000-0002-000000000001', 'empty-204',
            decode(repeat('04', 32), 'hex'), 'completed', 204, '', ''::bytea,
            now() + interval '1 day')",
    )
    .await
    .unwrap();

    let database_backend = db.get_database_backend();
    assert_eq!(database_backend, sea_orm::DbBackend::Postgres);
}
