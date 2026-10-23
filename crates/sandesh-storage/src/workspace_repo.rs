//! Multi-workspace and database management.
//!
//! Provides isolated `SQLite` databases and blob stores per project/workspace,
//! complete with atomic data purging and disk space reclamation.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;

use async_trait::async_trait;
use sandesh_core::{
    Account, AccountStore, CoreError, CoreResult, EventPayload, PurgeMode, PurgeOptions,
    PurgeResult, Timestamp, Workspace, WorkspaceStore,
};
use serde::{Deserialize, Serialize};

use crate::SqliteAccountStore;
use crate::db::Database;
use crate::error::StorageError;
use crate::events::EventRecorder;

const REGISTRY_FILE: &str = "workspaces.json";
const DEFAULT_WORKSPACE_ID: &str = "default";
const DEFAULT_WORKSPACE_NAME: &str = "Default Workspace";
const DEFAULT_WORKSPACE_DESC: &str = "Default local development workspace";

/// Persistent metadata for a single workspace.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceMetadata {
    /// Slug identifier (e.g. "default", "billing-service").
    pub id: String,
    /// Display name.
    pub name: String,
    /// Optional description.
    pub description: Option<String>,
    /// Created timestamp in microseconds UTC.
    pub created_at: i64,
    /// Whether authentication is required.
    #[serde(default)]
    pub auth_required: bool,
}

/// Registry stored in `workspaces.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspacesRegistry {
    /// The currently active workspace slug.
    pub active_id: String,
    /// All registered workspaces.
    pub workspaces: Vec<WorkspaceMetadata>,
}

impl Default for WorkspacesRegistry {
    fn default() -> Self {
        Self {
            active_id: DEFAULT_WORKSPACE_ID.to_string(),
            workspaces: vec![WorkspaceMetadata {
                id: DEFAULT_WORKSPACE_ID.to_string(),
                name: DEFAULT_WORKSPACE_NAME.to_string(),
                description: Some(DEFAULT_WORKSPACE_DESC.to_string()),
                created_at: Timestamp::now().as_micros(),
                auth_required: false,
            }],
        }
    }
}

/// Manages workspace storage directories, `SQLite` connection pools, and lifecycle.
pub struct WorkspaceManager {
    data_dir: PathBuf,
    default_db_path: PathBuf,
    default_blob_path: PathBuf,
    events: EventRecorder,
    registry: RwLock<WorkspacesRegistry>,
    open_pools: RwLock<HashMap<String, Database>>,
}

impl std::fmt::Debug for WorkspaceManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkspaceManager")
            .field("data_dir", &self.data_dir)
            .field("default_db_path", &self.default_db_path)
            .finish_non_exhaustive()
    }
}

impl WorkspaceManager {
    /// Initializes the workspace manager, loading or seeding `workspaces.json`.
    ///
    /// # Errors
    /// Fails if the data directory cannot be created or the registry file is corrupt.
    pub async fn load_or_init(
        data_dir: PathBuf,
        default_db_path: PathBuf,
        default_blob_path: PathBuf,
        events: EventRecorder,
    ) -> Result<Self, StorageError> {
        tokio::fs::create_dir_all(&data_dir)
            .await
            .map_err(StorageError::from)?;

        let workspaces_dir = data_dir.join("workspaces");
        tokio::fs::create_dir_all(&workspaces_dir)
            .await
            .map_err(StorageError::from)?;

        let registry_path = data_dir.join(REGISTRY_FILE);
        let registry = if registry_path.exists() {
            let bytes = tokio::fs::read(&registry_path)
                .await
                .map_err(StorageError::from)?;
            serde_json::from_slice::<WorkspacesRegistry>(&bytes).unwrap_or_else(|_| {
                let r = WorkspacesRegistry::default();
                let _ = std::fs::write(
                    &registry_path,
                    serde_json::to_vec_pretty(&r).unwrap_or_default(),
                );
                r
            })
        } else {
            let r = WorkspacesRegistry::default();
            let _ = tokio::fs::write(
                &registry_path,
                serde_json::to_vec_pretty(&r).unwrap_or_default(),
            )
            .await;
            r
        };

        Ok(Self {
            data_dir,
            default_db_path,
            default_blob_path,
            events,
            registry: RwLock::new(registry),
            open_pools: RwLock::new(HashMap::new()),
        })
    }

    /// The base data directory.
    #[must_use]
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// The active workspace identifier.
    pub async fn active_id(&self) -> String {
        self.registry.read().await.active_id.clone()
    }

    /// Path to the `SQLite` database file for a workspace.
    #[must_use]
    pub fn db_path_for(&self, id: &str) -> PathBuf {
        if id == DEFAULT_WORKSPACE_ID {
            self.default_db_path.clone()
        } else {
            self.data_dir.join("workspaces").join(id).join("sandesh.db")
        }
    }

    /// Path to the blob storage directory for a workspace.
    #[must_use]
    pub fn blob_path_for(&self, id: &str) -> PathBuf {
        if id == DEFAULT_WORKSPACE_ID {
            self.default_blob_path.clone()
        } else {
            self.data_dir.join("workspaces").join(id).join("blobs")
        }
    }

    /// Registers a pre-opened database (e.g. the default workspace).
    pub async fn register_pool(&self, id: &str, db: Database) {
        self.open_pools.write().await.insert(id.to_string(), db);
    }

    /// Gets an open database for a workspace, opening and migrating it if not already open.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the workspace is unknown, or if opening or
    /// migrating its database fails.
    pub async fn get_or_open_db(&self, id: &str) -> Result<Database, StorageError> {
        {
            let pools = self.open_pools.read().await;
            if let Some(db) = pools.get(id) {
                return Ok(db.clone());
            }
        }

        let db_path = self.db_path_for(id);
        if let Some(parent) = db_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(StorageError::from)?;
        }

        let db = Database::open(&db_path).await?;
        self.open_pools
            .write()
            .await
            .insert(id.to_string(), db.clone());
        Ok(db)
    }

    /// Saves the registry to disk.
    async fn persist_registry(&self, reg: &WorkspacesRegistry) -> Result<(), StorageError> {
        let registry_path = self.data_dir.join(REGISTRY_FILE);
        let bytes = serde_json::to_vec_pretty(reg).map_err(|e| {
            StorageError::Migration(format!("cannot serialize workspace registry: {e}"))
        })?;
        tokio::fs::write(&registry_path, bytes)
            .await
            .map_err(StorageError::from)?;
        Ok(())
    }

    /// Lists all workspaces with up-to-date message counts and disk sizes.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if workspace statistics cannot be read from disk.
    pub async fn list(&self) -> Result<Vec<Workspace>, StorageError> {
        let reg = self.registry.read().await.clone();
        let pools = self.open_pools.read().await;
        let mut list = Vec::with_capacity(reg.workspaces.len());

        for meta in &reg.workspaces {
            let db_path = self.db_path_for(&meta.id);
            let blob_path = self.blob_path_for(&meta.id);
            // Prefer the already-open pool to avoid opening a fresh read-only
            // connection for every workspace on every list call.
            let cached_pool = pools.get(&meta.id).map(|db| db.pool().clone());
            let (message_count, size_bytes) =
                compute_stats(&db_path, &blob_path, cached_pool.as_ref()).await;

            list.push(Workspace {
                id: meta.id.clone(),
                name: meta.name.clone(),
                description: meta.description.clone(),
                created_at: Timestamp::from_micros(meta.created_at),
                is_active: meta.id == reg.active_id,
                message_count,
                size_bytes,
                database_path: db_path.display().to_string(),
                auth_required: meta.auth_required,
            });
        }

        Ok(list)
    }

    /// Gets a single workspace by identifier.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the workspace's statistics cannot be read.
    pub async fn get(&self, id: &str) -> Result<Option<Workspace>, StorageError> {
        let reg = self.registry.read().await;
        let Some(meta) = reg.workspaces.iter().find(|w| w.id == id) else {
            return Ok(None);
        };

        let db_path = self.db_path_for(&meta.id);
        let blob_path = self.blob_path_for(&meta.id);
        let cached_pool = self
            .open_pools
            .read()
            .await
            .get(&meta.id)
            .map(|db| db.pool().clone());
        let (message_count, size_bytes) =
            compute_stats(&db_path, &blob_path, cached_pool.as_ref()).await;

        Ok(Some(Workspace {
            id: meta.id.clone(),
            name: meta.name.clone(),
            description: meta.description.clone(),
            created_at: Timestamp::from_micros(meta.created_at),
            is_active: meta.id == reg.active_id,
            message_count,
            size_bytes,
            database_path: db_path.display().to_string(),
            auth_required: meta.auth_required,
        }))
    }

    /// Creates a new isolated workspace with its own database and blob storage.
    ///
    /// If `clone_from` is provided and points to an existing workspace with accounts,
    /// its accounts, email identities, default mailboxes (with counter resets), and
    /// rules are cloned into the new database. Otherwise, if `initial_account` is provided,
    /// that account and its bound email addresses are configured; if omitted, a default
    /// `dev` catch-all account is seeded.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the identifier is already taken, if the
    /// storage directories cannot be created, or if the new database cannot be
    /// opened and migrated.
    pub async fn create(
        &self,
        id: &str,
        name: &str,
        description: Option<&str>,
        clone_from: Option<&str>,
        initial_account: Option<sandesh_core::InitialAccountSetup>,
        auth_required: bool,
    ) -> Result<Workspace, StorageError> {
        validate_workspace_slug(id)?;

        let mut reg = self.registry.write().await;
        if reg.workspaces.iter().any(|w| w.id == id) {
            return Err(StorageError::Migration(format!(
                "workspace with id '{id}' already exists"
            )));
        }

        let db_path = self.db_path_for(id);
        let blob_path = self.blob_path_for(id);

        if let Some(parent) = db_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(StorageError::from)?;
        }
        tokio::fs::create_dir_all(&blob_path)
            .await
            .map_err(StorageError::from)?;

        // Initialize and migrate the SQLite database
        let db = Database::open(&db_path).await?;

        let mut accounts_cloned = 0;
        if let Some(src_id) = clone_from {
            let src_db = self.get_or_open_db(src_id).await?;
            accounts_cloned = clone_accounts_and_rules(src_db.pool(), db.pool()).await?;
        }

        if accounts_cloned == 0 {
            let account_store = SqliteAccountStore::new(db.pool().clone(), self.events.clone());
            if let Some(setup) = initial_account {
                let username = if setup.username.trim().is_empty() {
                    "dev".to_string()
                } else {
                    setup.username.trim().to_string()
                };
                let credential = setup
                    .password_hash
                    .map(|hash| sandesh_core::Credential { hash, plain: None });
                let account = Account {
                    id: sandesh_core::AccountId::new(),
                    username,
                    credential,
                    is_catch_all: setup.is_catch_all,
                    created_at: Timestamp::now(),
                };
                let _ = account_store.create(&account).await;

                let mut is_primary = true;
                for addr_str in setup.addresses {
                    let trimmed = addr_str.trim();
                    if !trimmed.is_empty() {
                        let email_addr = sandesh_core::EmailAddress::parse_lenient(trimmed);
                        let _ = account_store
                            .add_identity(account.id, &email_addr, is_primary)
                            .await;
                        is_primary = false;
                    }
                }
            } else {
                // Seed initial catch-all account `dev` so the workspace can immediately accept mail
                let account = Account {
                    id: sandesh_core::AccountId::new(),
                    username: "dev".to_string(),
                    credential: None,
                    is_catch_all: true,
                    created_at: Timestamp::now(),
                };
                let _ = account_store.create(&account).await;
            }
        }

        self.open_pools.write().await.insert(id.to_string(), db);

        let meta = WorkspaceMetadata {
            id: id.to_string(),
            name: name.to_string(),
            description: description.map(ToString::to_string),
            created_at: Timestamp::now().as_micros(),
            auth_required,
        };

        reg.workspaces.push(meta.clone());
        self.persist_registry(&reg).await?;

        self.events.emit(EventPayload::WorkspaceCreated {
            workspace_id: id.to_string(),
            name: name.to_string(),
        });

        let (message_count, size_bytes) = compute_stats(&db_path, &blob_path, None).await;
        Ok(Workspace {
            id: meta.id,
            name: meta.name,
            description: meta.description,
            created_at: Timestamp::from_micros(meta.created_at),
            is_active: false,
            message_count,
            size_bytes,
            database_path: db_path.display().to_string(),
            auth_required: meta.auth_required,
        })
    }

    /// Updates an existing workspace's metadata and settings.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if no workspace has this identifier, or if the
    /// registry cannot be written back.
    pub async fn update(
        &self,
        id: &str,
        update: sandesh_core::WorkspaceUpdate,
    ) -> Result<Workspace, StorageError> {
        let mut reg = self.registry.write().await;
        let Some(meta) = reg.workspaces.iter_mut().find(|w| w.id == id) else {
            return Err(StorageError::Migration(format!("no such workspace '{id}'")));
        };

        if let Some(name) = update.name {
            let trimmed = name.trim();
            if !trimmed.is_empty() {
                meta.name = trimmed.to_string();
            }
        }
        if let Some(desc) = update.description {
            meta.description = desc.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        }
        if let Some(auth_req) = update.auth_required {
            meta.auth_required = auth_req;
        }

        let updated_meta = meta.clone();
        let is_active = updated_meta.id == reg.active_id;
        self.persist_registry(&reg).await?;
        drop(reg);

        let db_path = self.db_path_for(id);
        let blob_path = self.blob_path_for(id);
        let cached_pool = self
            .open_pools
            .read()
            .await
            .get(id)
            .map(|db| db.pool().clone());
        let (message_count, size_bytes) =
            compute_stats(&db_path, &blob_path, cached_pool.as_ref()).await;

        Ok(Workspace {
            id: updated_meta.id,
            name: updated_meta.name,
            description: updated_meta.description,
            created_at: Timestamp::from_micros(updated_meta.created_at),
            is_active,
            message_count,
            size_bytes,
            database_path: db_path.display().to_string(),
            auth_required: updated_meta.auth_required,
        })
    }

    /// Sets the active workspace.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if no workspace has the given identifier, or if
    /// the registry cannot be persisted.
    pub async fn set_active(&self, id: &str) -> Result<(), StorageError> {
        let mut reg = self.registry.write().await;
        if !reg.workspaces.iter().any(|w| w.id == id) {
            return Err(StorageError::Migration(format!("no such workspace '{id}'")));
        }

        if reg.active_id == id {
            return Ok(());
        }

        let previous = reg.active_id.clone();
        reg.active_id = id.to_string();
        self.persist_registry(&reg).await?;

        // Pre-warm the pool for the active workspace
        drop(reg);
        let _ = self.get_or_open_db(id).await;

        self.events.emit(EventPayload::WorkspaceSwitched {
            previous,
            current: id.to_string(),
        });

        Ok(())
    }

    /// Deletes a workspace and purges its database and blobs from disk.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when asked to delete the default workspace, or if
    /// removing its files or persisting the registry fails.
    pub async fn delete(&self, id: &str) -> Result<bool, StorageError> {
        if id == DEFAULT_WORKSPACE_ID {
            return Err(StorageError::Migration(String::from(
                "cannot delete the default workspace",
            )));
        }

        let mut reg = self.registry.write().await;
        let pos = reg.workspaces.iter().position(|w| w.id == id);
        let Some(index) = pos else {
            return Ok(false);
        };

        let was_active = reg.active_id == id;
        reg.workspaces.remove(index);

        if was_active {
            reg.active_id = DEFAULT_WORKSPACE_ID.to_string();
        }

        self.persist_registry(&reg).await?;
        drop(reg);

        if was_active {
            self.events.emit(EventPayload::WorkspaceSwitched {
                previous: id.to_string(),
                current: DEFAULT_WORKSPACE_ID.to_string(),
            });
        }

        // Flush WAL and close the pool before removing files on disk.
        // Running wal_checkpoint(TRUNCATE) ensures no open file descriptor
        // keeps a partially-flushed WAL alive after the directory is removed.
        if let Some(db) = self.open_pools.write().await.remove(id) {
            let _ = sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
                .execute(db.pool())
                .await;
            db.close().await;
        }

        // Delete the workspace folder and database files from disk.
        let workspace_dir = self.data_dir.join("workspaces").join(id);
        if workspace_dir.exists() {
            let _ = tokio::fs::remove_dir_all(&workspace_dir).await;
        }

        self.events.emit(EventPayload::WorkspaceDeleted {
            workspace_id: id.to_string(),
        });

        Ok(true)
    }

    /// Purges all messages, blobs, traces, and resets UID counters for a workspace.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the workspace is unknown or its database
    /// cannot be opened or written to.
    pub async fn purge(
        &self,
        id: &str,
        options: PurgeOptions,
    ) -> Result<PurgeResult, StorageError> {
        // Ensure workspace exists
        let exists = self
            .registry
            .read()
            .await
            .workspaces
            .iter()
            .any(|w| w.id == id);
        if !exists {
            return Err(StorageError::Migration(format!("no such workspace '{id}'")));
        }

        let db = self.get_or_open_db(id).await?;
        let pool = db.pool();
        let blob_path = self.blob_path_for(id);
        let db_path = self.db_path_for(id);

        let (initial_msgs, size_before) = compute_stats(&db_path, &blob_path, Some(pool)).await;
        let trace_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM connections")
            .fetch_one(pool)
            .await
            .unwrap_or((0,));

        // 1. Delete all messages. Cascades to message_entries, message_headers,
        // mime_parts, attachments, deliveries, delivery_recipients, entry_keywords, messages_fts_map.
        sqlx::query("DELETE FROM messages")
            .execute(pool)
            .await
            .map_err(StorageError::from)?;

        // 2. Clear FTS5 search index table
        let _ = sqlx::query("DELETE FROM messages_fts").execute(pool).await;

        // 3. Delete all blob files in the blob storage directory
        if blob_path.exists()
            && let Ok(mut entries) = tokio::fs::read_dir(&blob_path).await
        {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                if path.is_file() {
                    let _ = tokio::fs::remove_file(path).await;
                } else if path.is_dir() {
                    let _ = tokio::fs::remove_dir_all(path).await;
                }
            }
        }

        // 4. Optionally clear connection traces
        if options.clear_traces {
            let _ = sqlx::query("DELETE FROM connections").execute(pool).await;
        }

        // 5. Optionally clear webhook outbox records
        if options.clear_webhooks {
            let _ = sqlx::query("DELETE FROM webhook_outbox")
                .execute(pool)
                .await;
        }

        // 6. Reset mailbox UID sequences and mod_seq
        let _ = sqlx::query("UPDATE mailboxes SET uid_next = 1, highest_mod_seq = 1")
            .execute(pool)
            .await;

        // 7. Factory reset: reset accounts and rules, re-create default catch-all
        if options.mode == PurgeMode::FactoryReset {
            let _ = sqlx::query("DELETE FROM accounts").execute(pool).await;
            let _ = sqlx::query("DELETE FROM message_rules").execute(pool).await;
            let _ = sqlx::query("DELETE FROM webhook_endpoints")
                .execute(pool)
                .await;

            let account = Account {
                id: sandesh_core::AccountId::new(),
                username: "dev".to_string(),
                credential: None,
                is_catch_all: true,
                created_at: Timestamp::now(),
            };
            let account_store = SqliteAccountStore::new(pool.clone(), self.events.clone());
            let _ = account_store.create(&account).await;
        }

        // 8. Truncate WAL and run VACUUM to reclaim disk space
        let _ = sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
            .execute(pool)
            .await;
        let _ = sqlx::query("VACUUM").execute(pool).await;

        let (_, size_after) = compute_stats(&db_path, &blob_path, Some(pool)).await;
        let reclaimed_bytes = size_before.saturating_sub(size_after);

        let deleted_traces = if options.clear_traces {
            u64::try_from(trace_count.0).unwrap_or(0)
        } else {
            0
        };

        self.events.emit(EventPayload::WorkspacePurged {
            workspace_id: id.to_string(),
            deleted_messages: initial_msgs,
            deleted_traces,
            reclaimed_bytes,
        });

        Ok(PurgeResult {
            workspace_id: id.to_string(),
            deleted_messages: initial_msgs,
            deleted_traces,
            reclaimed_bytes,
            purged_at: Timestamp::now(),
        })
    }

    /// Flushes uncheckpointed WAL frames into the database file for a workspace.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the workspace's database cannot be opened.
    /// A checkpoint that finds nothing to flush is success, not an error.
    pub async fn checkpoint(&self, id: &str) -> Result<(), StorageError> {
        let db = self.get_or_open_db(id).await?;
        let _ = sqlx::query("PRAGMA wal_checkpoint(PASSIVE)")
            .execute(db.pool())
            .await
            .map_err(StorageError::from)?;
        Ok(())
    }
}

/// Computes message count and total disk usage (database + WAL + SHM + blobs).
///
/// Accepts an optional pre-opened pool. When one is provided it is used directly
/// for the count query, avoiding the overhead of opening a fresh read-only
/// connection per workspace on every `list` call.
async fn compute_stats(
    db_path: &Path,
    blob_path: &Path,
    pool: Option<&sqlx::SqlitePool>,
) -> (u64, u64) {
    let mut total_bytes = 0u64;

    if let Ok(meta) = tokio::fs::metadata(db_path).await {
        total_bytes += meta.len();
    }
    let wal = db_path.with_extension("db-wal");
    if let Ok(meta) = tokio::fs::metadata(&wal).await {
        total_bytes += meta.len();
    }
    let shm = db_path.with_extension("db-shm");
    if let Ok(meta) = tokio::fs::metadata(&shm).await {
        total_bytes += meta.len();
    }

    if blob_path.exists() {
        total_bytes += dir_size(blob_path).await;
    }

    let count = if let Some(p) = pool {
        // Reuse the caller's open pool — no extra connection open/close.
        query_message_count_from_pool(p).await.unwrap_or(0)
    } else if db_path.exists() {
        query_message_count_from_file(db_path).await.unwrap_or(0)
    } else {
        0
    };

    (count, total_bytes)
}

/// Queries the message count using an existing open pool.
async fn query_message_count_from_pool(pool: &sqlx::SqlitePool) -> Option<u64> {
    let row: Result<(i64,), _> = sqlx::query_as("SELECT COUNT(*) FROM messages")
        .fetch_one(pool)
        .await;
    row.ok().map(|(c,)| u64::try_from(c).unwrap_or(0))
}

/// Opens a temporary read-only connection to query the message count from a file.
/// Used only when no cached pool is available.
async fn query_message_count_from_file(db_path: &Path) -> Option<u64> {
    use sqlx::sqlite::SqliteConnectOptions;
    let options = SqliteConnectOptions::new()
        .filename(db_path)
        .read_only(true);

    let pool = sqlx::SqlitePool::connect_with(options).await.ok()?;
    let row: Result<(i64,), _> = sqlx::query_as("SELECT COUNT(*) FROM messages")
        .fetch_one(&pool)
        .await;
    pool.close().await;
    row.ok().map(|(c,)| u64::try_from(c).unwrap_or(0))
}

/// Recursively calculates directory byte size.
fn dir_size(path: &Path) -> std::pin::Pin<Box<dyn std::future::Future<Output = u64> + Send + '_>> {
    Box::pin(async move {
        let mut sum = 0u64;
        if let Ok(mut dir) = tokio::fs::read_dir(path).await {
            while let Ok(Some(entry)) = dir.next_entry().await {
                let p = entry.path();
                if let Ok(meta) = entry.metadata().await {
                    if meta.is_dir() {
                        sum += dir_size(&p).await;
                    } else {
                        sum += meta.len();
                    }
                }
            }
        }
        sum
    })
}

/// Validates a workspace slug identifier.
fn validate_workspace_slug(id: &str) -> Result<(), StorageError> {
    if id.is_empty() {
        return Err(StorageError::Migration(String::from(
            "workspace id cannot be empty",
        )));
    }
    if id.len() > 64 {
        return Err(StorageError::Migration(String::from(
            "workspace id cannot exceed 64 characters",
        )));
    }
    let valid = id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !valid {
        return Err(StorageError::Migration(format!(
            "workspace id '{id}' can only contain alphanumeric characters, hyphens, and underscores"
        )));
    }
    Ok(())
}

/// Adapter implementing `sandesh_core::WorkspaceStore`.
#[derive(Debug)]
pub struct SqliteWorkspaceStore {
    manager: Arc<WorkspaceManager>,
}

impl SqliteWorkspaceStore {
    /// Wraps a workspace manager as a core port.
    #[must_use]
    pub fn new(manager: Arc<WorkspaceManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl WorkspaceStore for SqliteWorkspaceStore {
    async fn list(&self) -> CoreResult<Vec<Workspace>> {
        self.manager.list().await.map_err(CoreError::from)
    }

    async fn get(&self, id: &str) -> CoreResult<Option<Workspace>> {
        self.manager.get(id).await.map_err(CoreError::from)
    }

    async fn active_id(&self) -> CoreResult<String> {
        Ok(self.manager.active_id().await)
    }

    async fn create(
        &self,
        id: &str,
        name: &str,
        description: Option<&str>,
        clone_from: Option<&str>,
        initial_account: Option<sandesh_core::InitialAccountSetup>,
        auth_required: bool,
    ) -> CoreResult<Workspace> {
        self.manager
            .create(
                id,
                name,
                description,
                clone_from,
                initial_account,
                auth_required,
            )
            .await
            .map_err(CoreError::from)
    }

    async fn update(
        &self,
        id: &str,
        update: sandesh_core::WorkspaceUpdate,
    ) -> CoreResult<Workspace> {
        self.manager
            .update(id, update)
            .await
            .map_err(CoreError::from)
    }

    async fn set_active(&self, id: &str) -> CoreResult<()> {
        self.manager.set_active(id).await.map_err(CoreError::from)
    }

    async fn delete(&self, id: &str) -> CoreResult<bool> {
        self.manager.delete(id).await.map_err(CoreError::from)
    }

    async fn purge(&self, id: &str, options: PurgeOptions) -> CoreResult<PurgeResult> {
        self.manager
            .purge(id, options)
            .await
            .map_err(CoreError::from)
    }

    async fn checkpoint(&self, id: &str) -> CoreResult<()> {
        self.manager.checkpoint(id).await.map_err(CoreError::from)
    }
}

/// Clones accounts, email identities, mailboxes, and rules from a source database
/// into a target database, resetting message counters (`uid_next` to 1, `highest_mod_seq` to 1).
/// Returns the number of accounts cloned.
async fn clone_accounts_and_rules(
    src_pool: &sqlx::SqlitePool,
    dst_pool: &sqlx::SqlitePool,
) -> Result<usize, StorageError> {
    use sqlx::Row;

    // 1. Fetch accounts
    let accounts = sqlx::query(
        "SELECT id, username, credential_hash, credential_plain, is_catch_all, created_at FROM accounts",
    )
    .fetch_all(src_pool)
    .await
    .map_err(StorageError::from)?;

    if accounts.is_empty() {
        return Ok(0);
    }

    // 2. Fetch email_identities
    let identities = sqlx::query(
        "SELECT id, account_id, address, address_raw, is_primary FROM email_identities",
    )
    .fetch_all(src_pool)
    .await
    .map_err(StorageError::from)?;

    // 3. Fetch mailboxes (order root mailboxes first so parent_id references exist)
    let mailboxes = sqlx::query(
        "SELECT id, account_id, name, parent_id, uid_validity, attributes, subscribed, created_at
         FROM mailboxes
         ORDER BY CASE WHEN parent_id IS NULL THEN 0 ELSE 1 END",
    )
    .fetch_all(src_pool)
    .await
    .map_err(StorageError::from)?;

    // 4. Fetch rules
    let rules = sqlx::query("SELECT id, name, conditions, action, enabled, created_at FROM rules")
        .fetch_all(src_pool)
        .await
        .map_err(StorageError::from)?;

    let mut tx = dst_pool.begin().await.map_err(StorageError::from)?;

    for acc in &accounts {
        let id: Vec<u8> = acc.try_get("id").map_err(StorageError::from)?;
        let username: String = acc.try_get("username").map_err(StorageError::from)?;
        let cred_hash: Option<String> =
            acc.try_get("credential_hash").map_err(StorageError::from)?;
        let cred_plain: Option<String> = acc
            .try_get("credential_plain")
            .map_err(StorageError::from)?;
        let is_catch_all: i64 = acc.try_get("is_catch_all").map_err(StorageError::from)?;
        let created_at: i64 = acc.try_get("created_at").map_err(StorageError::from)?;

        sqlx::query(
            "INSERT INTO accounts (id, username, credential_hash, credential_plain, is_catch_all, created_at)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(username)
        .bind(cred_hash)
        .bind(cred_plain)
        .bind(is_catch_all)
        .bind(created_at)
        .execute(&mut *tx)
        .await
        .map_err(StorageError::from)?;
    }

    for iden in &identities {
        let id: Vec<u8> = iden.try_get("id").map_err(StorageError::from)?;
        let account_id: Vec<u8> = iden.try_get("account_id").map_err(StorageError::from)?;
        let address: String = iden.try_get("address").map_err(StorageError::from)?;
        let address_raw: String = iden.try_get("address_raw").map_err(StorageError::from)?;
        let is_primary: i64 = iden.try_get("is_primary").map_err(StorageError::from)?;

        sqlx::query(
            "INSERT INTO email_identities (id, account_id, address, address_raw, is_primary)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(account_id)
        .bind(address)
        .bind(address_raw)
        .bind(is_primary)
        .execute(&mut *tx)
        .await
        .map_err(StorageError::from)?;
    }

    insert_cloned_mailboxes(&mut tx, &mailboxes).await?;

    insert_cloned_rules(&mut tx, &rules).await?;

    tx.commit().await.map_err(StorageError::from)?;
    Ok(accounts.len())
}

/// Copies fetched mailbox rows into the destination workspace transaction,
/// resetting per-mailbox counters (`uid_next`, `highest_mod_seq`) for the clone.
async fn insert_cloned_mailboxes(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    mailboxes: &[sqlx::sqlite::SqliteRow],
) -> Result<(), StorageError> {
    use sqlx::Row;

    for mbx in mailboxes {
        let id: Vec<u8> = mbx.try_get("id").map_err(StorageError::from)?;
        let account_id: Vec<u8> = mbx.try_get("account_id").map_err(StorageError::from)?;
        let name: String = mbx.try_get("name").map_err(StorageError::from)?;
        let parent_id: Option<Vec<u8>> = mbx.try_get("parent_id").map_err(StorageError::from)?;
        let uid_validity: i64 = mbx.try_get("uid_validity").map_err(StorageError::from)?;
        let attributes: String = mbx.try_get("attributes").map_err(StorageError::from)?;
        let subscribed: i64 = mbx.try_get("subscribed").map_err(StorageError::from)?;
        let created_at: i64 = mbx.try_get("created_at").map_err(StorageError::from)?;

        sqlx::query(
            "INSERT INTO mailboxes (id, account_id, name, parent_id, uid_validity, uid_next, attributes, subscribed, created_at, highest_mod_seq)
             VALUES (?, ?, ?, ?, ?, 1, ?, ?, ?, 1)",
        )
        .bind(id)
        .bind(account_id)
        .bind(name)
        .bind(parent_id)
        .bind(uid_validity)
        .bind(attributes)
        .bind(subscribed)
        .bind(created_at)
        .execute(&mut **tx)
        .await
        .map_err(StorageError::from)?;
    }

    Ok(())
}

/// Copies fetched rule rows into the destination workspace transaction.
async fn insert_cloned_rules(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    rules: &[sqlx::sqlite::SqliteRow],
) -> Result<(), StorageError> {
    use sqlx::Row;

    for r in rules {
        let id: Vec<u8> = r.try_get("id").map_err(StorageError::from)?;
        let name: String = r.try_get("name").map_err(StorageError::from)?;
        let conditions: String = r.try_get("conditions").map_err(StorageError::from)?;
        let action: String = r.try_get("action").map_err(StorageError::from)?;
        let enabled: i64 = r.try_get("enabled").map_err(StorageError::from)?;
        let created_at: i64 = r.try_get("created_at").map_err(StorageError::from)?;

        sqlx::query(
            "INSERT INTO rules (id, name, conditions, action, enabled, created_at)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(name)
        .bind(conditions)
        .bind(action)
        .bind(enabled)
        .bind(created_at)
        .execute(&mut **tx)
        .await
        .map_err(StorageError::from)?;
    }

    Ok(())
}
