mod m20260710_000001_users_clients;
mod m20260710_000002_media_identity;
mod m20260710_000003_jobs;
mod m20260710_000004_idempotency_leases;
mod m20260711_000005_operation_receipts;
mod m20260712_000006_job_orchestration;
mod m20260712_000007_search_sessions;
mod m20260712_000008_tracking_notifications;
mod m20260712_000009_blocked_storage_notification;
mod m20260712_000010_search_scope;
mod m20260712_000011_notification_dead_letter;
mod m20260712_000012_progress_notifications;
mod m20260712_000013_session_refresh_notification;
mod m20260712_000014_runner_lifecycle;
mod m20260713_000015_job_retry;
mod m20260713_000016_parked_storage_jobs;
mod m20260713_000017_complete_terminal_stages;
mod m20260713_000018_completed_notification;
mod m20260713_000019_tracking_specials;
mod m20260713_000020_identity_episode_numbers;
mod m20260718_000021_notification_generation;
mod m20260718_000022_tracking_downloads;
mod m20260720_000023_sticky_vpn_attempts;
mod m20260720_000024_structured_notifications;
mod m20260721_000025_notification_episode_numbers;
mod m20260721_000026_notification_specials;
mod m20260723_000027_detailed_notifications;
mod m20260727_000028_source_choice_notifications;
mod m20260727_000029_availability_gated_tracking;
mod m20260727_000030_recheck_recent_calendar_discoveries;
mod m20260727_000031_remove_older_season_backfill;
mod m20260727_000032_tracking_availability_candidates;
mod m20260729_000033_tracking_controls;
mod m20260731_000034_notification_transfer_details;

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::DatabaseTransaction;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260710_000001_users_clients::Migration),
            Box::new(m20260710_000002_media_identity::Migration),
            Box::new(m20260710_000003_jobs::Migration),
            Box::new(m20260710_000004_idempotency_leases::Migration),
            Box::new(m20260711_000005_operation_receipts::Migration),
            Box::new(m20260712_000006_job_orchestration::Migration),
            Box::new(m20260712_000007_search_sessions::Migration),
            Box::new(m20260712_000008_tracking_notifications::Migration),
            Box::new(m20260712_000009_blocked_storage_notification::Migration),
            Box::new(m20260712_000010_search_scope::Migration),
            Box::new(m20260712_000011_notification_dead_letter::Migration),
            Box::new(m20260712_000012_progress_notifications::Migration),
            Box::new(m20260712_000013_session_refresh_notification::Migration),
            Box::new(m20260712_000014_runner_lifecycle::Migration),
            Box::new(m20260713_000015_job_retry::Migration),
            Box::new(m20260713_000016_parked_storage_jobs::Migration),
            Box::new(m20260713_000017_complete_terminal_stages::Migration),
            Box::new(m20260713_000018_completed_notification::Migration),
            Box::new(m20260713_000019_tracking_specials::Migration),
            Box::new(m20260713_000020_identity_episode_numbers::Migration),
            Box::new(m20260718_000021_notification_generation::Migration),
            Box::new(m20260718_000022_tracking_downloads::Migration),
            Box::new(m20260720_000023_sticky_vpn_attempts::Migration),
            Box::new(m20260720_000024_structured_notifications::Migration),
            Box::new(m20260721_000025_notification_episode_numbers::Migration),
            Box::new(m20260721_000026_notification_specials::Migration),
            Box::new(m20260723_000027_detailed_notifications::Migration),
            Box::new(m20260727_000028_source_choice_notifications::Migration),
            Box::new(m20260727_000029_availability_gated_tracking::Migration),
            Box::new(m20260727_000030_recheck_recent_calendar_discoveries::Migration),
            Box::new(m20260727_000031_remove_older_season_backfill::Migration),
            Box::new(m20260727_000032_tracking_availability_candidates::Migration),
            Box::new(m20260729_000033_tracking_controls::Migration),
            Box::new(m20260731_000034_notification_transfer_details::Migration),
        ]
    }
}

/// Finishes the migration body's nested transaction explicitly.
///
/// Each migration also opts into SeaORM's outer transaction so its ledger row
/// remains atomic with the committed DDL.
async fn finish_transaction(
    transaction: DatabaseTransaction,
    result: Result<(), DbErr>,
) -> Result<(), DbErr> {
    match result {
        Ok(()) => transaction.commit().await,
        Err(error) => match transaction.rollback().await {
            Ok(()) => Err(error),
            Err(rollback_error) => Err(DbErr::Migration(format!(
                "migration operation failed: {error}; explicit rollback failed: {rollback_error}"
            ))),
        },
    }
}
