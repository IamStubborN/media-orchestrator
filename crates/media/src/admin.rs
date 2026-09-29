use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};

use media_api::{MediaAdminError, MediaAdminService};
use media_core::{Actor, UserId};
use media_integrations::{plex::PlexClient, qbittorrent::QbittorrentClient};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

const CONFIRMATION_TTL: Duration = Duration::from_secs(10 * 60);

#[derive(Clone)]
enum DestructiveAction {
    PlexDelete { rating_key: u64 },
    TorrentDelete { hash: String, delete_files: bool },
    FileQuarantine { path: PathBuf },
}

struct Confirmation {
    owner: UserId,
    action: DestructiveAction,
    fingerprint: String,
    expires_at: Instant,
}

pub(crate) struct MediaAdminAdapter {
    plex: Option<Arc<PlexClient>>,
    qbittorrent: Option<Arc<QbittorrentClient>>,
    plex_sections: Vec<u32>,
    roots: Vec<PathBuf>,
    quarantine_root: PathBuf,
    confirmations: Mutex<HashMap<String, Confirmation>>,
}

impl MediaAdminAdapter {
    pub(crate) fn new(
        plex: Option<Arc<PlexClient>>,
        qbittorrent: Option<Arc<QbittorrentClient>>,
        plex_sections: Vec<u32>,
        roots: Vec<PathBuf>,
        quarantine_root: PathBuf,
    ) -> Self {
        Self {
            plex,
            qbittorrent,
            plex_sections,
            roots,
            quarantine_root,
            confirmations: Mutex::new(HashMap::new()),
        }
    }

    fn user(actor: &Actor) -> Result<UserId, MediaAdminError> {
        actor.require_user().map_err(|_| MediaAdminError::Forbidden)
    }

    fn plex(&self) -> Result<&PlexClient, MediaAdminError> {
        self.plex.as_deref().ok_or(MediaAdminError::Unavailable)
    }

    fn qbittorrent(&self) -> Result<&QbittorrentClient, MediaAdminError> {
        self.qbittorrent
            .as_deref()
            .ok_or(MediaAdminError::Unavailable)
    }

    async fn scoped_path(&self, raw: &str) -> Result<PathBuf, MediaAdminError> {
        let path = Path::new(raw);
        if !path.is_absolute() {
            return Err(MediaAdminError::InvalidRequest);
        }
        let canonical = tokio::fs::canonicalize(path).await.map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                MediaAdminError::NotFound
            } else {
                MediaAdminError::Provider
            }
        })?;
        for root in &self.roots {
            if let Ok(root) = tokio::fs::canonicalize(root).await
                && canonical.starts_with(root)
            {
                return Ok(canonical);
            }
        }
        Err(MediaAdminError::Forbidden)
    }

    async fn path_fingerprint(path: &Path) -> Result<String, MediaAdminError> {
        let metadata = tokio::fs::metadata(path)
            .await
            .map_err(|_| MediaAdminError::NotFound)?;
        let modified = metadata
            .modified()
            .ok()
            .and_then(|value| value.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map_or(0, |value| value.as_secs());
        Ok(format!(
            "{}:{}:{}:{}",
            path.display(),
            metadata.len(),
            modified,
            metadata.is_dir()
        ))
    }

    fn value_fingerprint(value: &Value) -> String {
        let bytes = serde_json::to_vec(value).unwrap_or_default();
        Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    async fn store_confirmation(
        &self,
        owner: UserId,
        action: DestructiveAction,
        fingerprint: String,
        preview: Value,
    ) -> Value {
        let token = uuid::Uuid::new_v4().to_string();
        let expires_at = Instant::now() + CONFIRMATION_TTL;
        let mut confirmations = self.confirmations.lock().await;
        confirmations.retain(|_, value| value.expires_at > Instant::now());
        confirmations.insert(
            token.clone(),
            Confirmation {
                owner,
                action,
                fingerprint,
                expires_at,
            },
        );
        json!({ "requires_confirmation": true, "confirmation_token": token, "expires_in_seconds": CONFIRMATION_TTL.as_secs(), "preview": preview })
    }
}

#[async_trait::async_trait]
impl MediaAdminService for MediaAdminAdapter {
    async fn plex_search(
        &self,
        actor: &Actor,
        query: &str,
        limit: u16,
    ) -> Result<Value, MediaAdminError> {
        Self::user(actor)?;
        self.plex()?
            .admin_search(query, limit)
            .await
            .map_err(map_plex)
    }
    async fn plex_recent(&self, actor: &Actor, limit: u16) -> Result<Value, MediaAdminError> {
        Self::user(actor)?;
        self.plex()?.admin_recent(limit).await.map_err(map_plex)
    }
    async fn plex_library_summary(&self, actor: &Actor) -> Result<Value, MediaAdminError> {
        Self::user(actor)?;
        self.plex()?
            .admin_library_summary(&self.plex_sections)
            .await
            .map_err(map_plex)
    }
    async fn plex_library_items(
        &self,
        actor: &Actor,
        section_key: u32,
        start: u32,
        limit: u16,
    ) -> Result<Value, MediaAdminError> {
        Self::user(actor)?;
        if !self.plex_sections.contains(&section_key) {
            return Err(MediaAdminError::Forbidden);
        }
        self.plex()?
            .admin_library_items(section_key, start, limit)
            .await
            .map_err(map_plex)
    }
    async fn plex_now_playing(&self, actor: &Actor) -> Result<Value, MediaAdminError> {
        Self::user(actor)?;
        self.plex()?.admin_now_playing().await.map_err(map_plex)
    }
    async fn plex_item(&self, actor: &Actor, rating_key: u64) -> Result<Value, MediaAdminError> {
        Self::user(actor)?;
        self.plex()?.admin_item(rating_key).await.map_err(map_plex)
    }
    async fn plex_refresh(
        &self,
        actor: &Actor,
        section_key: u32,
    ) -> Result<Value, MediaAdminError> {
        Self::user(actor)?;
        if !self.plex_sections.contains(&section_key) {
            return Err(MediaAdminError::Forbidden);
        }
        self.plex()?
            .admin_refresh(section_key)
            .await
            .map_err(map_plex)?;
        tracing::info!(section_key, "media admin requested Plex refresh");
        Ok(json!({ "accepted": true, "section_key": section_key }))
    }
    async fn qbittorrent_list(
        &self,
        actor: &Actor,
        filter: Option<&str>,
    ) -> Result<Value, MediaAdminError> {
        Self::user(actor)?;
        self.qbittorrent()?
            .admin_list(filter)
            .await
            .map_err(map_qbit)
    }
    async fn qbittorrent_details(
        &self,
        actor: &Actor,
        hash: &str,
    ) -> Result<Value, MediaAdminError> {
        Self::user(actor)?;
        self.qbittorrent()?
            .admin_details(hash)
            .await
            .map_err(map_qbit)
    }
    async fn qbittorrent_control(
        &self,
        actor: &Actor,
        hash: &str,
        action: &str,
    ) -> Result<Value, MediaAdminError> {
        Self::user(actor)?;
        self.qbittorrent()?
            .admin_control(hash, action)
            .await
            .map_err(map_qbit)?;
        tracing::info!(hash, action, "media admin controlled torrent");
        Ok(json!({ "accepted": true, "hash": hash, "action": action }))
    }
    async fn file_inspect(&self, actor: &Actor, path: &str) -> Result<Value, MediaAdminError> {
        Self::user(actor)?;
        let path = self.scoped_path(path).await?;
        let metadata = tokio::fs::metadata(&path)
            .await
            .map_err(|_| MediaAdminError::NotFound)?;
        let mut entries = Vec::new();
        if metadata.is_dir() {
            let mut reader = tokio::fs::read_dir(&path)
                .await
                .map_err(|_| MediaAdminError::Provider)?;
            while entries.len() < 200 {
                let Some(entry) = reader
                    .next_entry()
                    .await
                    .map_err(|_| MediaAdminError::Provider)?
                else {
                    break;
                };
                let metadata = entry
                    .metadata()
                    .await
                    .map_err(|_| MediaAdminError::Provider)?;
                entries.push(json!({ "name": entry.file_name().to_string_lossy(), "size_bytes": metadata.len(), "directory": metadata.is_dir() }));
            }
        }
        Ok(
            json!({ "path": path, "size_bytes": metadata.len(), "directory": metadata.is_dir(), "entries": entries, "truncated": entries.len() == 200 }),
        )
    }
    async fn infrastructure_status(&self, actor: &Actor) -> Result<Value, MediaAdminError> {
        Self::user(actor)?;
        let plex = match self.plex() {
            Ok(client) => client.admin_recent(1).await.is_ok(),
            Err(_) => false,
        };
        let qbittorrent = match self.qbittorrent() {
            Ok(client) => client.admin_list(Some("active")).await.is_ok(),
            Err(_) => false,
        };
        Ok(
            json!({ "media_service": "ready", "plex": if plex { "ready" } else { "unavailable" }, "qbittorrent": if qbittorrent { "ready" } else { "unavailable" }, "docker_socket_exposed": false }),
        )
    }
    async fn storage_status(&self, actor: &Actor) -> Result<Value, MediaAdminError> {
        Self::user(actor)?;
        let mut roots = Vec::with_capacity(self.roots.len());
        for configured_root in &self.roots {
            let root = tokio::fs::canonicalize(configured_root)
                .await
                .map_err(|_| MediaAdminError::Provider)?;
            let measured_root = root.clone();
            let (total_bytes, available_bytes) = tokio::task::spawn_blocking(move || {
                Ok::<_, std::io::Error>((
                    fs2::total_space(&measured_root)?,
                    fs2::available_space(&measured_root)?,
                ))
            })
            .await
            .map_err(|_| MediaAdminError::Provider)?
            .map_err(|_| MediaAdminError::Provider)?;
            let used_bytes = total_bytes.saturating_sub(available_bytes);
            let used_percent = used_bytes
                .saturating_mul(100)
                .checked_div(total_bytes)
                .unwrap_or(0);
            roots.push(json!({
                "path": root,
                "total_bytes": total_bytes,
                "available_bytes": available_bytes,
                "used_bytes": used_bytes,
                "used_percent": used_percent,
            }));
        }
        Ok(json!({ "roots": roots }))
    }
    async fn prepare_destructive(
        &self,
        actor: &Actor,
        action: &str,
        target: &str,
        delete_files: bool,
    ) -> Result<Value, MediaAdminError> {
        let owner = Self::user(actor)?;
        match action {
            "plex_delete" => {
                let rating_key = target
                    .parse::<u64>()
                    .map_err(|_| MediaAdminError::InvalidRequest)?;
                let item = self
                    .plex()?
                    .admin_item(rating_key)
                    .await
                    .map_err(map_plex)?;
                let fingerprint = Self::value_fingerprint(&item);
                self.store_confirmation(owner, DestructiveAction::PlexDelete { rating_key }, fingerprint, json!({ "action": action, "rating_key": rating_key, "item": item, "warning": "Plex will remove this library item and may delete its media files when server deletion is enabled." })).await.pipe(Ok)
            }
            "torrent_delete" => {
                let details = self
                    .qbittorrent()?
                    .admin_details(target)
                    .await
                    .map_err(map_qbit)?;
                let fingerprint = Self::value_fingerprint(&details);
                self.store_confirmation(owner, DestructiveAction::TorrentDelete { hash: target.to_owned(), delete_files }, fingerprint, json!({ "action": action, "hash": target, "delete_files": delete_files, "torrent": details })).await.pipe(Ok)
            }
            "file_quarantine" => {
                let path = self.scoped_path(target).await?;
                let fingerprint = Self::path_fingerprint(&path).await?;
                self.store_confirmation(
                    owner,
                    DestructiveAction::FileQuarantine { path: path.clone() },
                    fingerprint,
                    json!({ "action": action, "path": path, "destination": self.quarantine_root }),
                )
                .await
                .pipe(Ok)
            }
            _ => Err(MediaAdminError::InvalidRequest),
        }
    }
    async fn confirm_destructive(
        &self,
        actor: &Actor,
        confirmation_token: &str,
    ) -> Result<Value, MediaAdminError> {
        let owner = Self::user(actor)?;
        let confirmation = self
            .confirmations
            .lock()
            .await
            .remove(confirmation_token)
            .ok_or(MediaAdminError::InvalidConfirmation)?;
        if confirmation.owner != owner || confirmation.expires_at <= Instant::now() {
            return Err(MediaAdminError::InvalidConfirmation);
        }
        match confirmation.action {
            DestructiveAction::PlexDelete { rating_key } => {
                let current = self
                    .plex()?
                    .admin_item(rating_key)
                    .await
                    .map_err(map_plex)?;
                if Self::value_fingerprint(&current) != confirmation.fingerprint {
                    return Err(MediaAdminError::InvalidConfirmation);
                }
                self.plex()?
                    .admin_delete(rating_key)
                    .await
                    .map_err(map_plex)?;
                tracing::warn!(owner = %owner, rating_key, "media admin deleted Plex item");
                Ok(json!({ "completed": true, "action": "plex_delete", "rating_key": rating_key }))
            }
            DestructiveAction::TorrentDelete { hash, delete_files } => {
                let current = self
                    .qbittorrent()?
                    .admin_details(&hash)
                    .await
                    .map_err(map_qbit)?;
                if Self::value_fingerprint(&current) != confirmation.fingerprint {
                    return Err(MediaAdminError::InvalidConfirmation);
                }
                self.qbittorrent()?
                    .admin_delete(&hash, delete_files)
                    .await
                    .map_err(map_qbit)?;
                tracing::warn!(owner = %owner, hash, delete_files, "media admin deleted torrent");
                Ok(
                    json!({ "completed": true, "action": "torrent_delete", "hash": hash, "files_deleted": delete_files }),
                )
            }
            DestructiveAction::FileQuarantine { path } => {
                if Self::path_fingerprint(&path).await? != confirmation.fingerprint {
                    return Err(MediaAdminError::InvalidConfirmation);
                }
                tokio::fs::create_dir_all(&self.quarantine_root)
                    .await
                    .map_err(|_| MediaAdminError::Provider)?;
                let name = path.file_name().ok_or(MediaAdminError::InvalidRequest)?;
                let destination = self.quarantine_root.join(format!(
                    "{}-{}",
                    uuid::Uuid::new_v4(),
                    name.to_string_lossy()
                ));
                tokio::fs::rename(&path, &destination)
                    .await
                    .map_err(|_| MediaAdminError::Provider)?;
                tracing::warn!(owner = %owner, source = %path.display(), destination = %destination.display(), "media admin quarantined file");
                Ok(
                    json!({ "completed": true, "action": "file_quarantine", "source": path, "destination": destination }),
                )
            }
        }
    }
}

fn map_plex(error: media_integrations::plex::PlexError) -> MediaAdminError {
    match error.code() {
        media_integrations::plex::PlexErrorCode::InvalidRequest => MediaAdminError::InvalidRequest,
        media_integrations::plex::PlexErrorCode::Configuration => MediaAdminError::Unavailable,
        _ => MediaAdminError::Provider,
    }
}

fn map_qbit(error: media_integrations::qbittorrent::QbittorrentError) -> MediaAdminError {
    match error.code() {
        media_integrations::qbittorrent::QbittorrentErrorCode::InvalidSelection => {
            MediaAdminError::InvalidRequest
        }
        media_integrations::qbittorrent::QbittorrentErrorCode::TorrentNotFound => {
            MediaAdminError::NotFound
        }
        media_integrations::qbittorrent::QbittorrentErrorCode::Configuration => {
            MediaAdminError::Unavailable
        }
        _ => MediaAdminError::Provider,
    }
}

trait Pipe: Sized {
    fn pipe<T>(self, f: impl FnOnce(Self) -> T) -> T {
        f(self)
    }
}
impl<T> Pipe for T {}

#[cfg(test)]
mod tests {
    use media_api::MediaAdminService;
    use media_core::{
        Actor, ClientRole, PRIMARY_CLIENT_ID, PRIMARY_USER_ID, SECONDARY_CLIENT_ID,
        SECONDARY_USER_ID,
    };

    use super::MediaAdminAdapter;

    #[tokio::test]
    async fn storage_status_reports_bounded_capacity_for_configured_roots() {
        let root = std::env::temp_dir().join(format!("media-admin-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let adapter = MediaAdminAdapter::new(
            None,
            None,
            Vec::new(),
            vec![root.clone()],
            root.join("quarantine"),
        );
        let actor =
            Actor::new(PRIMARY_CLIENT_ID, Some(PRIMARY_USER_ID), ClientRole::Hermes).unwrap();

        let status = adapter.storage_status(&actor).await.unwrap();

        let item = &status["roots"][0];
        let canonical_root = tokio::fs::canonicalize(&root).await.unwrap();
        assert_eq!(item["path"], canonical_root.to_string_lossy().as_ref());
        assert!(item["total_bytes"].as_u64().unwrap() > 0);
        assert!(item["available_bytes"].as_u64().unwrap() <= item["total_bytes"].as_u64().unwrap());
        assert!(item["used_percent"].as_u64().unwrap() <= 100);
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn every_hermes_owner_can_prepare_and_confirm_destructive_actions() {
        let root = std::env::temp_dir().join(format!("media-admin-{}", uuid::Uuid::new_v4()));
        let quarantine = root.join("quarantine");
        let media_file = root.join("episode.mkv");
        tokio::fs::create_dir_all(&root).await.unwrap();
        tokio::fs::write(&media_file, b"media").await.unwrap();
        let adapter = MediaAdminAdapter::new(
            None,
            None,
            Vec::new(),
            vec![root.clone()],
            quarantine.clone(),
        );
        let actor = Actor::new(
            SECONDARY_CLIENT_ID,
            Some(SECONDARY_USER_ID),
            ClientRole::Hermes,
        )
        .unwrap();

        let prepared = adapter
            .prepare_destructive(
                &actor,
                "file_quarantine",
                media_file.to_str().unwrap(),
                false,
            )
            .await
            .unwrap();
        assert_eq!(prepared["requires_confirmation"], true);

        let confirmed = adapter
            .confirm_destructive(&actor, prepared["confirmation_token"].as_str().unwrap())
            .await
            .unwrap();
        assert_eq!(confirmed["completed"], true);
        assert!(!media_file.exists());
        assert!(
            std::path::Path::new(confirmed["destination"].as_str().unwrap()).exists(),
            "confirmed destination must exist"
        );
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
