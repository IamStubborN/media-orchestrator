mod support;

use media_storage::{MaintenanceReport, SeaOrmMaintenanceStore};
use sea_orm::ConnectionTrait;
use support::{TestDatabase, query};
use time::{Duration, OffsetDateTime};

const PRIMARY_ID: &str = "00000000-0000-0000-0000-000000000001";

fn timestamp(value: OffsetDateTime) -> String {
    value
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap()
}

#[tokio::test]
async fn run_removes_expired_sessions_and_only_unreferenced_executions() {
    let database = TestDatabase::start_migrated().await;
    let now = OffsetDateTime::from_unix_timestamp(2_000_000_000).unwrap();
    let old = timestamp(now - Duration::hours(25));
    let future = timestamp(now + Duration::hours(1));

    database
        .connection()
        .execute_unprepared(&format!(
            "INSERT INTO search_sessions (id, owner_id, payload, expires_at, created_at) VALUES
             ('10000000-0000-0000-0000-000000000001', '{PRIMARY_ID}', '{{}}', '{future}', '{old}'),
             ('10000000-0000-0000-0000-000000000002', '{PRIMARY_ID}', '{{}}', '{future}', '{future}');
             INSERT INTO search_executions (result_ref, payload, created_at) VALUES
             ('selection:expired-unreferenced', '{{}}', '{old}'),
             ('selection:expired-referenced', '{{}}', '{old}'),
             ('selection:fresh-unreferenced', '{{}}', '{future}');
             INSERT INTO jobs (id, owner_id, provider, result_ref, state, notify_scope, created_at)
             VALUES ('20000000-0000-0000-0000-000000000001', '{PRIMARY_ID}', 'rezka',
                     'selection:expired-referenced', 'queued', 'initiator', '{old}')"
        ))
        .await
        .unwrap();

    let report = SeaOrmMaintenanceStore::new(database.connection().clone())
        .run(now)
        .await
        .unwrap();

    assert_eq!(
        report,
        MaintenanceReport {
            search_sessions_deleted: 1,
            search_executions_deleted: 1,
            jobs_deleted: 0,
            notifications_deleted: 0,
            outbox_events_deleted: 0,
        }
    );
    assert_eq!(
        query(database.connection(), "SELECT id FROM search_sessions")
            .await
            .len(),
        1
    );
    let executions = query(
        database.connection(),
        "SELECT result_ref FROM search_executions ORDER BY result_ref",
    )
    .await;
    assert_eq!(executions.len(), 2);
    assert_eq!(
        executions[0].try_get::<String>("", "result_ref").unwrap(),
        "selection:expired-referenced"
    );
    assert_eq!(
        executions[1].try_get::<String>("", "result_ref").unwrap(),
        "selection:fresh-unreferenced"
    );
}

#[tokio::test]
async fn run_removes_only_old_terminal_job_history_and_preserves_media() {
    let database = TestDatabase::start_migrated().await;
    let now = OffsetDateTime::from_unix_timestamp(2_000_000_000).unwrap();
    let old = timestamp(now - Duration::days(91));
    let recent = timestamp(now - Duration::days(89));

    database
        .connection()
        .execute_unprepared(&format!(
            "INSERT INTO media (id, kind, title) VALUES
         ('30000000-0000-0000-0000-000000000001', 'movie', 'Published Movie');
         INSERT INTO search_executions (result_ref, payload, created_at) VALUES
         ('selection:old-terminal', '{{}}', '{old}');
         INSERT INTO jobs
           (id, owner_id, provider, result_ref, state, notify_scope, created_at, completed_at)
         VALUES
           ('20000000-0000-0000-0000-000000000011', '{PRIMARY_ID}', 'rezka',
            'selection:old-terminal', 'completed', 'initiator', '{old}', '{old}'),
           ('20000000-0000-0000-0000-000000000012', '{PRIMARY_ID}', 'rezka',
            'selection:recent-terminal', 'failed', 'initiator', '{recent}', '{recent}'),
           ('20000000-0000-0000-0000-000000000013', '{PRIMARY_ID}', 'rezka',
            'selection:old-queued', 'queued', 'initiator', '{old}', NULL)"
        ))
        .await
        .unwrap();

    let store = SeaOrmMaintenanceStore::new(database.connection().clone());
    let report = store.run(now).await.unwrap();

    assert_eq!(report.jobs_deleted, 1);
    assert_eq!(report.search_executions_deleted, 1);
    assert_eq!(
        query(database.connection(), "SELECT id FROM jobs")
            .await
            .len(),
        2
    );
    assert_eq!(
        query(database.connection(), "SELECT id FROM media")
            .await
            .len(),
        1
    );
    assert_eq!(store.run(now).await.unwrap(), MaintenanceReport::default());
}

#[tokio::test]
async fn run_never_removes_active_or_nonterminal_jobs() {
    let database = TestDatabase::start_migrated().await;
    let now = OffsetDateTime::from_unix_timestamp(2_000_000_000).unwrap();
    let old = timestamp(now - Duration::days(91));
    let future = timestamp(now + Duration::minutes(5));

    database.connection().execute_unprepared(&format!(
        "INSERT INTO api_clients (id, name, role, credential_digest) VALUES
         ('00000000-0000-0000-0002-000000000001', 'runner', 'runner', decode(repeat('ab', 32), 'hex'));
         INSERT INTO jobs
           (id, owner_id, provider, result_ref, state, notify_scope, created_at, completed_at)
         VALUES
           ('20000000-0000-0000-0000-000000000021', '{PRIMARY_ID}', 'rezka',
            'selection:old-with-lease', 'completed', 'initiator', '{old}', '{old}'),
           ('20000000-0000-0000-0000-000000000022', '{PRIMARY_ID}', 'rezka',
            'selection:old-queued', 'queued', 'initiator', '{old}', NULL),
           ('20000000-0000-0000-0000-000000000023', '{PRIMARY_ID}', 'rezka',
            'selection:old-running', 'running', 'initiator', '{old}', NULL);
         INSERT INTO job_leases (id, slot, job_id, runner_client_id, expires_at, created_at)
         VALUES ('40000000-0000-0000-0000-000000000001', 1,
                 '20000000-0000-0000-0000-000000000021',
                 '00000000-0000-0000-0002-000000000001', '{future}', '{old}')"
    )).await.unwrap();

    let report = SeaOrmMaintenanceStore::new(database.connection().clone())
        .run(now)
        .await
        .unwrap();

    assert_eq!(report.jobs_deleted, 0);
    assert_eq!(
        query(database.connection(), "SELECT id FROM jobs")
            .await
            .len(),
        3
    );
    assert_eq!(
        query(database.connection(), "SELECT id FROM job_leases")
            .await
            .len(),
        1
    );
}

#[tokio::test]
async fn run_removes_an_expired_lease_before_pruning_its_old_terminal_job() {
    let database = TestDatabase::start_migrated().await;
    let now = OffsetDateTime::from_unix_timestamp(2_000_000_000).unwrap();
    let old = timestamp(now - Duration::days(91));
    let expired = timestamp(now - Duration::minutes(1));

    database.connection().execute_unprepared(&format!(
        "INSERT INTO api_clients (id, name, role, credential_digest) VALUES
         ('00000000-0000-0000-0002-000000000001', 'runner', 'runner', decode(repeat('cd', 32), 'hex'));
         INSERT INTO jobs
           (id, owner_id, provider, result_ref, state, notify_scope, created_at, completed_at)
         VALUES ('20000000-0000-0000-0000-000000000031', '{PRIMARY_ID}', 'rezka',
                 'selection:old-expired-lease', 'partial', 'initiator', '{old}', '{old}');
         INSERT INTO job_leases (id, slot, job_id, runner_client_id, expires_at, created_at)
         VALUES ('40000000-0000-0000-0000-000000000011', 1,
                 '20000000-0000-0000-0000-000000000031',
                 '00000000-0000-0000-0002-000000000001', '{expired}', '{old}')"
    )).await.unwrap();

    let report = SeaOrmMaintenanceStore::new(database.connection().clone())
        .run(now)
        .await
        .unwrap();

    assert_eq!(report.jobs_deleted, 1);
    assert!(
        query(database.connection(), "SELECT id FROM jobs")
            .await
            .is_empty()
    );
    assert!(
        query(database.connection(), "SELECT id FROM job_leases")
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn run_prunes_old_delivered_and_job_notifications_without_touching_recent_pending() {
    let database = TestDatabase::start_migrated().await;
    let now = OffsetDateTime::from_unix_timestamp(2_000_000_000).unwrap();
    let old = timestamp(now - Duration::days(91));
    let recent = timestamp(now - Duration::days(1));

    database
        .connection()
        .execute_unprepared(&format!(
            "INSERT INTO jobs
               (id, owner_id, provider, result_ref, state, notify_scope, created_at, completed_at)
             VALUES
               ('20000000-0000-0000-0000-000000000041', '{PRIMARY_ID}', 'rezka',
                'selection:old-notification', 'failed', 'initiator', '{old}', '{old}'),
               ('20000000-0000-0000-0000-000000000042', '{PRIMARY_ID}', 'rezka',
                'selection:leased-notification', 'failed', 'initiator', '{old}', '{old}');
             INSERT INTO notification_outbox
               (id, aggregate_type, aggregate_id, event_type, recipient, source_dedupe_key,
                payload, delivered_at, created_at)
             VALUES
               ('50000000-0000-0000-0000-000000000001', 'job',
                '20000000-0000-0000-0000-000000000041', 'failed', 'primary', decode('01', 'hex'),
                '{{\"message\":\"old pending\"}}', NULL, '{old}'),
               ('50000000-0000-0000-0000-000000000002', 'tracking',
                '60000000-0000-0000-0000-000000000001', 'future-episode-found', 'primary',
                decode('02', 'hex'), '{{\"message\":\"old delivered\"}}', '{old}', '{old}'),
               ('50000000-0000-0000-0000-000000000003', 'tracking',
                '60000000-0000-0000-0000-000000000002', 'future-episode-found', 'primary',
                decode('03', 'hex'), '{{\"message\":\"recent pending\"}}', NULL, '{recent}'),
               ('50000000-0000-0000-0000-000000000004', 'job',
                '20000000-0000-0000-0000-000000000042', 'failed', 'primary',
                decode('05', 'hex'), '{{\"message\":\"leased old delivery\"}}', NULL, '{old}');
             UPDATE notification_outbox
             SET lease_owner = '80000000-0000-0000-0000-000000000001',
                 lease_expires_at = '{recent}'::timestamptz + interval '2 days'
             WHERE id = '50000000-0000-0000-0000-000000000004';
             INSERT INTO outbox_events
               (id, aggregate_type, aggregate_id, event_type, dedupe_key, payload, created_at)
             VALUES ('70000000-0000-0000-0000-000000000001', 'job',
                     '20000000-0000-0000-0000-000000000041', 'job.failed',
                     decode('04', 'hex'), '{{}}', '{old}')"
        ))
        .await
        .unwrap();

    let report = SeaOrmMaintenanceStore::new(database.connection().clone())
        .run(now)
        .await
        .unwrap();

    assert_eq!(report.jobs_deleted, 1);
    assert_eq!(report.notifications_deleted, 2);
    assert_eq!(report.outbox_events_deleted, 1);
    let notifications = query(
        database.connection(),
        "SELECT payload->>'message' AS message FROM notification_outbox",
    )
    .await;
    let messages = notifications
        .iter()
        .map(|row| row.try_get::<String>("", "message").unwrap())
        .collect::<Vec<_>>();
    assert_eq!(messages.len(), 2);
    assert!(messages.contains(&"recent pending".to_owned()));
    assert!(messages.contains(&"leased old delivery".to_owned()));
    let jobs = query(database.connection(), "SELECT id FROM jobs").await;
    assert_eq!(jobs.len(), 1);
    assert_eq!(
        jobs[0].try_get::<uuid::Uuid>("", "id").unwrap(),
        uuid::Uuid::parse_str("20000000-0000-0000-0000-000000000042").unwrap()
    );
    assert!(
        query(database.connection(), "SELECT id FROM outbox_events")
            .await
            .is_empty()
    );
}
