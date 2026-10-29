//! Dynamic storage proxy delegating to the currently active workspace.
//!
//! Enables zero-downtime workspace switching: when a developer switches the active
//! workspace, subsequent mail deliveries and API calls are automatically routed to
//! the newly selected workspace's database pool without restarting listeners.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use async_trait::async_trait;
use sandesh_api::state::ReadinessProbe;
use sandesh_core::{
    Account, AccountId, AccountStore, Attachment, AttachmentId, BlobStore, ConnectionId,
    ConnectionRecord, ConnectionStore, CoreError, CoreResult, Delivery, DeliveryId, DeliveryStore,
    EmailAddress, EmailIdentity, EntryId, FlagOutcome, FlagUpdate, InsertedMessage, Mailbox,
    MailboxCounts, MailboxId, MailboxName, MailboxStore, Message, MessageEntry, MessageFlags,
    MessageId, MessageStore, MetricsSnapshot, MetricsSource, ModSeq, NewMessage, OutboxEntry,
    OutboxId, OutboxStatus, OutboxStore, Protocol, RawMessage, Rule, RuleId, RuleStore,
    SearchCandidates, SearchIndex, SearchQuery, Timestamp, Uid, WebhookEndpoint,
};
use sandesh_events::EventBus;
use sandesh_storage::{
    Database, EventRecorder, FilesystemBlobStore, SqliteConnectionStore, SqliteMetricsSource,
    SqliteOutboxStore, WorkspaceManager,
};

use crate::Repositories;

/// Repositories, pool, and blob store for an open workspace.
pub struct WorkspaceBundle {
    /// Active database pool.
    pub database: Database,
    /// Out-of-line blob storage.
    pub blobs: Arc<dyn BlobStore>,
    /// Repository ports over the workspace database.
    pub(crate) repos: Repositories,
    /// Connection transcripts store, if tracing enabled.
    pub connections: Option<Arc<dyn ConnectionStore>>,
    /// Webhook outbox store.
    pub outbox: Arc<dyn OutboxStore>,
    /// Metrics source.
    pub metrics: Arc<dyn MetricsSource>,
}

impl std::fmt::Debug for WorkspaceBundle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkspaceBundle")
            .field("database", &self.database)
            .finish_non_exhaustive()
    }
}

/// Dynamic storage router holding open workspace bundles.
pub struct DynamicStorageRouter {
    manager: Arc<WorkspaceManager>,
    _events: Arc<EventBus>,
    recorder: EventRecorder,
    tracing_enabled: bool,
    trace_retention: u32,
    bundles: RwLock<HashMap<String, Arc<WorkspaceBundle>>>,
    account_to_workspace: RwLock<HashMap<AccountId, String>>,
    mailbox_to_workspace: RwLock<HashMap<MailboxId, String>>,
}

impl std::fmt::Debug for DynamicStorageRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DynamicStorageRouter")
            .field("tracing_enabled", &self.tracing_enabled)
            .field("trace_retention", &self.trace_retention)
            .finish_non_exhaustive()
    }
}

/// Splits a username with an explicit workspace scope prefix, e.g. `billing-service/dev` or `billing-service:dev`.
#[must_use]
pub fn parse_scoped_username(username: &str) -> Option<(&str, &str)> {
    if let Some((ws, user)) = username.split_once('/') {
        let ws = ws.trim();
        let user = user.trim();
        if !ws.is_empty() && !user.is_empty() {
            return Some((ws, user));
        }
    }
    if let Some((ws, user)) = username.split_once(':') {
        let ws = ws.trim();
        let user = user.trim();
        if !ws.is_empty() && !user.is_empty() {
            return Some((ws, user));
        }
    }
    None
}

impl DynamicStorageRouter {
    /// Creates a new dynamic storage router.
    pub fn new(
        manager: Arc<WorkspaceManager>,
        events: Arc<EventBus>,
        recorder: EventRecorder,
        tracing_enabled: bool,
        trace_retention: u32,
    ) -> Self {
        Self {
            manager,
            _events: events,
            recorder,
            tracing_enabled,
            trace_retention,
            bundles: RwLock::new(HashMap::new()),
            account_to_workspace: RwLock::new(HashMap::new()),
            mailbox_to_workspace: RwLock::new(HashMap::new()),
        }
    }

    /// Registers a pre-opened bundle (e.g. the default workspace).
    pub async fn register_bundle(&self, id: &str, bundle: Arc<WorkspaceBundle>) {
        self.bundles.write().await.insert(id.to_string(), bundle);
    }

    /// The underlying workspace manager.
    #[must_use]
    pub fn manager(&self) -> &Arc<WorkspaceManager> {
        &self.manager
    }

    /// Records which workspace an account belongs to.
    pub async fn record_account_workspace(&self, account_id: AccountId, workspace: &str) {
        self.account_to_workspace
            .write()
            .await
            .insert(account_id, workspace.to_string());
    }

    /// Looks up which workspace an account belongs to, if cached.
    pub async fn workspace_for_account(&self, account_id: AccountId) -> Option<String> {
        self.account_to_workspace
            .read()
            .await
            .get(&account_id)
            .cloned()
    }

    /// Records which workspace a mailbox belongs to.
    pub async fn record_mailbox_workspace(&self, mailbox_id: MailboxId, workspace: &str) {
        self.mailbox_to_workspace
            .write()
            .await
            .insert(mailbox_id, workspace.to_string());
    }

    /// Looks up which workspace a mailbox belongs to, if cached.
    pub async fn workspace_for_mailbox(&self, mailbox_id: MailboxId) -> Option<String> {
        self.mailbox_to_workspace
            .read()
            .await
            .get(&mailbox_id)
            .cloned()
    }

    /// Resolves or opens the bundle for the currently active workspace.
    ///
    /// # Errors
    ///
    /// Returns an error if the active workspace's database cannot be opened.
    pub async fn active_bundle(&self) -> CoreResult<Arc<WorkspaceBundle>> {
        let active_id = self.manager.active_id().await;
        self.bundle_for(&active_id).await
    }

    /// Resolves or initializes the bundle for a given workspace id.
    ///
    /// Uses a double-checked locking pattern: we check the read lock first for
    /// the fast path, then acquire the write lock and check **again** before
    /// opening a new database pool. This prevents two concurrent first-accesses
    /// from both opening a pool, with the loser's pool being silently discarded.
    ///
    /// # Errors
    ///
    /// Returns an error if the workspace's database cannot be opened or migrated.
    pub async fn bundle_for(&self, id: &str) -> CoreResult<Arc<WorkspaceBundle>> {
        // Fast path — no allocation for the common case (bundle already open).
        {
            let bundles = self.bundles.read().await;
            if let Some(bundle) = bundles.get(id) {
                return Ok(Arc::clone(bundle));
            }
        }

        // Slow path — hold the write lock for the entire open+insert sequence
        // so that a concurrent caller that also missed the read lock above
        // will see the bundle we insert and return it rather than opening a
        // second pool.
        let mut bundles = self.bundles.write().await;

        // Re-check after acquiring the write lock (double-checked locking).
        if let Some(bundle) = bundles.get(id) {
            return Ok(Arc::clone(bundle));
        }

        let db = self
            .manager
            .get_or_open_db(id)
            .await
            .map_err(|e| CoreError::Storage(sandesh_core::StoreError::new(e.to_string())))?;

        let blob_path = self.manager.blob_path_for(id);
        let blobs: Arc<dyn BlobStore> = Arc::new(
            FilesystemBlobStore::new(&blob_path)
                .await
                .map_err(|e| CoreError::Storage(sandesh_core::StoreError::new(e.to_string())))?,
        );

        let pool = db.pool().clone();
        let repos = Repositories::new(&pool, &blobs, self.recorder.clone());
        crate::spawn_index_repair(&pool, &repos);

        let connections: Option<Arc<dyn ConnectionStore>> = self.tracing_enabled.then(|| {
            Arc::new(SqliteConnectionStore::new(pool.clone())) as Arc<dyn ConnectionStore>
        });

        let outbox: Arc<dyn OutboxStore> = Arc::new(SqliteOutboxStore::new(pool.clone()));

        let metrics: Arc<dyn MetricsSource> = Arc::new(SqliteMetricsSource::new(pool.clone()));

        let bundle = Arc::new(WorkspaceBundle {
            database: db,
            blobs,
            repos,
            connections,
            outbox,
            metrics,
        });

        bundles.insert(id.to_string(), Arc::clone(&bundle));
        Ok(bundle)
    }

    /// Invalidates/clears an open bundle cache when workspace is purged or reset.
    pub async fn invalidate(&self, id: &str) {
        self.bundles.write().await.remove(id);
        self.account_to_workspace
            .write()
            .await
            .retain(|_, ws| ws != id);
        self.mailbox_to_workspace
            .write()
            .await
            .retain(|_, ws| ws != id);
    }
}

// ---------------------------------------------------------------------------
// Dynamic delegating store adapters
// ---------------------------------------------------------------------------

/// Dynamic account store delegating to the active workspace.
#[derive(Clone, Debug)]
pub struct DynamicAccountStore {
    router: Arc<DynamicStorageRouter>,
}

impl DynamicAccountStore {
    /// Creates a new dynamic account store.
    #[must_use]
    pub fn new(router: Arc<DynamicStorageRouter>) -> Self {
        Self { router }
    }
}

#[async_trait]
impl AccountStore for DynamicAccountStore {
    async fn create(&self, account: &Account) -> CoreResult<()> {
        self.router
            .active_bundle()
            .await?
            .repos
            .accounts
            .create(account)
            .await
    }
    async fn get(&self, id: AccountId) -> CoreResult<Option<Account>> {
        if let Some(ws_id) = self.router.workspace_for_account(id).await
            && let Ok(bundle) = self.router.bundle_for(&ws_id).await
        {
            return bundle.repos.accounts.get(id).await;
        }
        self.router
            .active_bundle()
            .await?
            .repos
            .accounts
            .get(id)
            .await
    }
    async fn get_by_username(&self, username: &str) -> CoreResult<Option<Account>> {
        if let Some((ws_id, account_user)) = parse_scoped_username(username)
            && let Ok(bundle) = self.router.bundle_for(ws_id).await
            && let Ok(Some(account)) = bundle.repos.accounts.get_by_username(account_user).await
        {
            self.router
                .record_account_workspace(account.id, ws_id)
                .await;
            return Ok(Some(account));
        }

        let bundle = self.router.active_bundle().await?;
        let active_id = self.router.manager.active_id().await;
        let res = bundle.repos.accounts.get_by_username(username).await?;
        if let Some(ref account) = res {
            self.router
                .record_account_workspace(account.id, &active_id)
                .await;
        }
        Ok(res)
    }
    async fn list(&self) -> CoreResult<Vec<Account>> {
        self.router
            .active_bundle()
            .await?
            .repos
            .accounts
            .list()
            .await
    }
    async fn delete(&self, id: AccountId) -> CoreResult<bool> {
        self.router
            .active_bundle()
            .await?
            .repos
            .accounts
            .delete(id)
            .await
    }
    async fn add_identity(
        &self,
        account_id: AccountId,
        address: &EmailAddress,
        is_primary: bool,
    ) -> CoreResult<()> {
        self.router
            .active_bundle()
            .await?
            .repos
            .accounts
            .add_identity(account_id, address, is_primary)
            .await
    }
    async fn remove_identity(
        &self,
        account_id: AccountId,
        address: &EmailAddress,
    ) -> CoreResult<bool> {
        self.router
            .active_bundle()
            .await?
            .repos
            .accounts
            .remove_identity(account_id, address)
            .await
    }
    async fn identities(&self, account_id: AccountId) -> CoreResult<Vec<EmailIdentity>> {
        if let Some(ws_id) = self.router.workspace_for_account(account_id).await
            && let Ok(bundle) = self.router.bundle_for(&ws_id).await
        {
            return bundle.repos.accounts.identities(account_id).await;
        }
        self.router
            .active_bundle()
            .await?
            .repos
            .accounts
            .identities(account_id)
            .await
    }
    async fn resolve_address(&self, address: &EmailAddress) -> CoreResult<Option<AccountId>> {
        self.router
            .active_bundle()
            .await?
            .repos
            .accounts
            .resolve_address(address)
            .await
    }
    async fn catch_all(&self) -> CoreResult<Option<Account>> {
        self.router
            .active_bundle()
            .await?
            .repos
            .accounts
            .catch_all()
            .await
    }
    async fn update_credential(
        &self,
        id: AccountId,
        credential: Option<&sandesh_core::Credential>,
    ) -> CoreResult<bool> {
        self.router
            .active_bundle()
            .await?
            .repos
            .accounts
            .update_credential(id, credential)
            .await
    }
    async fn set_catch_all(&self, id: AccountId, is_catch_all: bool) -> CoreResult<bool> {
        self.router
            .active_bundle()
            .await?
            .repos
            .accounts
            .set_catch_all(id, is_catch_all)
            .await
    }
}

/// Dynamic mailbox store delegating to the active workspace.
#[derive(Clone, Debug)]
pub struct DynamicMailboxStore {
    router: Arc<DynamicStorageRouter>,
}

impl DynamicMailboxStore {
    /// Creates a new dynamic mailbox store.
    #[must_use]
    pub fn new(router: Arc<DynamicStorageRouter>) -> Self {
        Self { router }
    }
}

#[async_trait]
impl MailboxStore for DynamicMailboxStore {
    async fn create(&self, mailbox: &Mailbox) -> CoreResult<()> {
        self.router
            .active_bundle()
            .await?
            .repos
            .mailboxes
            .create(mailbox)
            .await
    }
    async fn get(&self, id: MailboxId) -> CoreResult<Option<Mailbox>> {
        if let Some(ws_id) = self.router.workspace_for_mailbox(id).await
            && let Ok(bundle) = self.router.bundle_for(&ws_id).await
        {
            return bundle.repos.mailboxes.get(id).await;
        }
        self.router
            .active_bundle()
            .await?
            .repos
            .mailboxes
            .get(id)
            .await
    }
    async fn get_by_name(
        &self,
        account_id: AccountId,
        name: &MailboxName,
    ) -> CoreResult<Option<Mailbox>> {
        let (bundle, ws_opt) =
            if let Some(ws_id) = self.router.workspace_for_account(account_id).await {
                (self.router.bundle_for(&ws_id).await?, Some(ws_id))
            } else {
                (self.router.active_bundle().await?, None)
            };
        let res = bundle.repos.mailboxes.get_by_name(account_id, name).await?;
        if let Some(ref m) = res
            && let Some(ws_id) = ws_opt
        {
            self.router.record_mailbox_workspace(m.id, &ws_id).await;
        }
        Ok(res)
    }
    async fn list(&self, account_id: AccountId) -> CoreResult<Vec<Mailbox>> {
        let (bundle, ws_opt) =
            if let Some(ws_id) = self.router.workspace_for_account(account_id).await {
                (self.router.bundle_for(&ws_id).await?, Some(ws_id))
            } else {
                (self.router.active_bundle().await?, None)
            };
        let mailboxes = bundle.repos.mailboxes.list(account_id).await?;
        if let Some(ws_id) = ws_opt {
            for m in &mailboxes {
                self.router.record_mailbox_workspace(m.id, &ws_id).await;
            }
        }
        Ok(mailboxes)
    }
    async fn rename(&self, id: MailboxId, new_name: &MailboxName) -> CoreResult<()> {
        self.router
            .active_bundle()
            .await?
            .repos
            .mailboxes
            .rename(id, new_name)
            .await
    }
    async fn delete(&self, id: MailboxId) -> CoreResult<bool> {
        self.router
            .active_bundle()
            .await?
            .repos
            .mailboxes
            .delete(id)
            .await
    }
    async fn set_subscribed(&self, id: MailboxId, subscribed: bool) -> CoreResult<()> {
        self.router
            .active_bundle()
            .await?
            .repos
            .mailboxes
            .set_subscribed(id, subscribed)
            .await
    }
    async fn counts(&self, id: MailboxId) -> CoreResult<MailboxCounts> {
        if let Some(ws_id) = self.router.workspace_for_mailbox(id).await
            && let Ok(bundle) = self.router.bundle_for(&ws_id).await
        {
            return bundle.repos.mailboxes.counts(id).await;
        }
        self.router
            .active_bundle()
            .await?
            .repos
            .mailboxes
            .counts(id)
            .await
    }
}

/// Dynamic message store delegating to the active workspace.
#[derive(Clone, Debug)]
pub struct DynamicMessageStore {
    router: Arc<DynamicStorageRouter>,
}

impl DynamicMessageStore {
    /// Creates a new dynamic message store.
    #[must_use]
    pub fn new(router: Arc<DynamicStorageRouter>) -> Self {
        Self { router }
    }
}

#[async_trait]
impl MessageStore for DynamicMessageStore {
    async fn insert(&self, message: &NewMessage) -> CoreResult<InsertedMessage> {
        if let Some(ref identity) = message.envelope.auth_identity
            && let Some((ws_id, _)) = parse_scoped_username(identity)
            && let Ok(bundle) = self.router.bundle_for(ws_id).await
        {
            return bundle.repos.messages.insert(message).await;
        }
        self.router
            .active_bundle()
            .await?
            .repos
            .messages
            .insert(message)
            .await
    }
    async fn get(&self, id: MessageId) -> CoreResult<Option<Message>> {
        self.router
            .active_bundle()
            .await?
            .repos
            .messages
            .get(id)
            .await
    }
    async fn raw(&self, id: MessageId) -> CoreResult<Option<RawMessage>> {
        self.router
            .active_bundle()
            .await?
            .repos
            .messages
            .raw(id)
            .await
    }
    async fn get_entry(&self, id: EntryId) -> CoreResult<Option<MessageEntry>> {
        self.router
            .active_bundle()
            .await?
            .repos
            .messages
            .get_entry(id)
            .await
    }
    async fn get_entry_by_uid(
        &self,
        mailbox_id: MailboxId,
        uid: Uid,
    ) -> CoreResult<Option<MessageEntry>> {
        if let Some(ws_id) = self.router.workspace_for_mailbox(mailbox_id).await
            && let Ok(bundle) = self.router.bundle_for(&ws_id).await
        {
            return bundle
                .repos
                .messages
                .get_entry_by_uid(mailbox_id, uid)
                .await;
        }
        self.router
            .active_bundle()
            .await?
            .repos
            .messages
            .get_entry_by_uid(mailbox_id, uid)
            .await
    }
    async fn list_entries(&self, mailbox_id: MailboxId) -> CoreResult<Vec<MessageEntry>> {
        if let Some(ws_id) = self.router.workspace_for_mailbox(mailbox_id).await
            && let Ok(bundle) = self.router.bundle_for(&ws_id).await
        {
            return bundle.repos.messages.list_entries(mailbox_id).await;
        }
        self.router
            .active_bundle()
            .await?
            .repos
            .messages
            .list_entries(mailbox_id)
            .await
    }
    async fn entries_for_message(&self, message_id: MessageId) -> CoreResult<Vec<MessageEntry>> {
        self.router
            .active_bundle()
            .await?
            .repos
            .messages
            .entries_for_message(message_id)
            .await
    }
    async fn page_entries(
        &self,
        mailboxes: &[MailboxId],
        before_uid: Option<u32>,
        limit: usize,
    ) -> CoreResult<sandesh_core::EntryPage> {
        // One workspace — the normal case — answers the whole page in SQL. A
        // scope spanning workspaces has no single database to ask, so it falls
        // back to the scan, which routes each mailbox through `list_entries`.
        let mut workspace: Option<Option<String>> = None;
        for &mailbox_id in mailboxes {
            let ws = self.router.workspace_for_mailbox(mailbox_id).await;
            match &workspace {
                None => workspace = Some(ws),
                Some(seen) if *seen == ws => {}
                Some(_) => {
                    return sandesh_core::page_entries_by_scan(self, mailboxes, before_uid, limit)
                        .await;
                }
            }
        }
        let bundle = match workspace.flatten() {
            Some(ws_id) => self.router.bundle_for(&ws_id).await?,
            None => self.router.active_bundle().await?,
        };
        bundle
            .repos
            .messages
            .page_entries(mailboxes, before_uid, limit)
            .await
    }
    async fn copy_entry(
        &self,
        entry_id: EntryId,
        target_mailbox: MailboxId,
    ) -> CoreResult<MessageEntry> {
        self.router
            .active_bundle()
            .await?
            .repos
            .messages
            .copy_entry(entry_id, target_mailbox)
            .await
    }
    async fn move_entry(
        &self,
        entry_id: EntryId,
        target_mailbox: MailboxId,
    ) -> CoreResult<MessageEntry> {
        self.router
            .active_bundle()
            .await?
            .repos
            .messages
            .move_entry(entry_id, target_mailbox)
            .await
    }
    async fn update_flags(
        &self,
        entry_id: EntryId,
        update: FlagUpdate,
    ) -> CoreResult<MessageFlags> {
        self.router
            .active_bundle()
            .await?
            .repos
            .messages
            .update_flags(entry_id, update)
            .await
    }
    async fn update_flags_conditional(
        &self,
        entry_id: EntryId,
        update: FlagUpdate,
        unchanged_since: Option<ModSeq>,
    ) -> CoreResult<FlagOutcome> {
        self.router
            .active_bundle()
            .await?
            .repos
            .messages
            .update_flags_conditional(entry_id, update, unchanged_since)
            .await
    }
    async fn delete_entry(&self, entry_id: EntryId) -> CoreResult<bool> {
        self.router
            .active_bundle()
            .await?
            .repos
            .messages
            .delete_entry(entry_id)
            .await
    }
    async fn collect_orphans(&self) -> CoreResult<Vec<MessageId>> {
        self.router
            .active_bundle()
            .await?
            .repos
            .messages
            .collect_orphans()
            .await
    }
    async fn attachments(&self, message_id: MessageId) -> CoreResult<Vec<Attachment>> {
        self.router
            .active_bundle()
            .await?
            .repos
            .messages
            .attachments(message_id)
            .await
    }
    async fn get_attachment(
        &self,
        id: AttachmentId,
    ) -> CoreResult<Option<(Attachment, MessageId)>> {
        self.router
            .active_bundle()
            .await?
            .repos
            .messages
            .get_attachment(id)
            .await
    }
}

/// Dynamic delivery store delegating to the active workspace.
#[derive(Clone, Debug)]
pub struct DynamicDeliveryStore {
    router: Arc<DynamicStorageRouter>,
}

impl DynamicDeliveryStore {
    /// Creates a new dynamic delivery store.
    #[must_use]
    pub fn new(router: Arc<DynamicStorageRouter>) -> Self {
        Self { router }
    }
}

#[async_trait]
impl DeliveryStore for DynamicDeliveryStore {
    async fn record(&self, delivery: &Delivery) -> CoreResult<()> {
        if let Some(ref identity) = delivery.envelope.auth_identity
            && let Some((ws_id, _)) = parse_scoped_username(identity)
            && let Ok(bundle) = self.router.bundle_for(ws_id).await
        {
            return bundle.repos.deliveries.record(delivery).await;
        }
        self.router
            .active_bundle()
            .await?
            .repos
            .deliveries
            .record(delivery)
            .await
    }
    async fn get(&self, id: DeliveryId) -> CoreResult<Option<Delivery>> {
        self.router
            .active_bundle()
            .await?
            .repos
            .deliveries
            .get(id)
            .await
    }
    async fn by_message_id(&self, message_id: MessageId) -> CoreResult<Option<Delivery>> {
        self.router
            .active_bundle()
            .await?
            .repos
            .deliveries
            .by_message_id(message_id)
            .await
    }
    async fn recent(&self, limit: u32) -> CoreResult<Vec<Delivery>> {
        self.router
            .active_bundle()
            .await?
            .repos
            .deliveries
            .recent(limit)
            .await
    }
}

/// Dynamic blob store delegating to the active workspace.
#[derive(Clone, Debug)]
pub struct DynamicBlobStore {
    router: Arc<DynamicStorageRouter>,
}

impl DynamicBlobStore {
    /// Creates a new dynamic blob store.
    #[must_use]
    pub fn new(router: Arc<DynamicStorageRouter>) -> Self {
        Self { router }
    }
}

#[async_trait]
impl BlobStore for DynamicBlobStore {
    async fn put(&self, data: &[u8]) -> CoreResult<String> {
        self.router.active_bundle().await?.blobs.put(data).await
    }
    async fn get(&self, reference: &str) -> CoreResult<Option<Vec<u8>>> {
        self.router
            .active_bundle()
            .await?
            .blobs
            .get(reference)
            .await
    }
    async fn delete(&self, reference: &str) -> CoreResult<bool> {
        self.router
            .active_bundle()
            .await?
            .blobs
            .delete(reference)
            .await
    }
}

/// Dynamic search index delegating to the active workspace.
#[derive(Clone, Debug)]
pub struct DynamicSearchIndex {
    router: Arc<DynamicStorageRouter>,
}

impl DynamicSearchIndex {
    /// Creates a new dynamic search index.
    #[must_use]
    pub fn new(router: Arc<DynamicStorageRouter>) -> Self {
        Self { router }
    }
}

#[async_trait]
impl SearchIndex for DynamicSearchIndex {
    async fn index(&self, message: &Message) -> CoreResult<()> {
        self.router
            .active_bundle()
            .await?
            .repos
            .search_index
            .index(message)
            .await
    }
    async fn remove(&self, id: MessageId) -> CoreResult<()> {
        self.router
            .active_bundle()
            .await?
            .repos
            .search_index
            .remove(id)
            .await
    }
    async fn candidates(&self, query: &SearchQuery) -> CoreResult<SearchCandidates> {
        self.router
            .active_bundle()
            .await?
            .repos
            .search_index
            .candidates(query)
            .await
    }
}

/// Dynamic rule store delegating to the active workspace.
#[derive(Clone, Debug)]
pub struct DynamicRuleStore {
    router: Arc<DynamicStorageRouter>,
}

impl DynamicRuleStore {
    /// Creates a new dynamic rule store.
    #[must_use]
    pub fn new(router: Arc<DynamicStorageRouter>) -> Self {
        Self { router }
    }
}

#[async_trait]
impl RuleStore for DynamicRuleStore {
    async fn save(&self, rule: &Rule) -> CoreResult<()> {
        self.router
            .active_bundle()
            .await?
            .repos
            .rules
            .save(rule)
            .await
    }
    async fn all(&self) -> CoreResult<Vec<Rule>> {
        self.router.active_bundle().await?.repos.rules.all().await
    }
    async fn remove(&self, id: RuleId) -> CoreResult<bool> {
        self.router
            .active_bundle()
            .await?
            .repos
            .rules
            .remove(id)
            .await
    }
}

/// Dynamic outbox store delegating to the active workspace.
#[derive(Clone, Debug)]
pub struct DynamicOutboxStore {
    router: Arc<DynamicStorageRouter>,
}

impl DynamicOutboxStore {
    /// Creates a new dynamic outbox store.
    #[must_use]
    pub fn new(router: Arc<DynamicStorageRouter>) -> Self {
        Self { router }
    }
}

#[async_trait]
impl OutboxStore for DynamicOutboxStore {
    async fn register_endpoint(&self, endpoint: &WebhookEndpoint) -> CoreResult<()> {
        self.router
            .active_bundle()
            .await?
            .outbox
            .register_endpoint(endpoint)
            .await
    }
    async fn endpoints(&self) -> CoreResult<Vec<WebhookEndpoint>> {
        self.router.active_bundle().await?.outbox.endpoints().await
    }
    async fn remove_endpoint(&self, id: sandesh_core::EndpointId) -> CoreResult<bool> {
        self.router
            .active_bundle()
            .await?
            .outbox
            .remove_endpoint(id)
            .await
    }
    async fn due(&self, now: Timestamp, limit: u32) -> CoreResult<Vec<OutboxEntry>> {
        self.router
            .active_bundle()
            .await?
            .outbox
            .due(now, limit)
            .await
    }
    async fn record_attempt(
        &self,
        id: OutboxId,
        status: OutboxStatus,
        next_attempt_at: Timestamp,
        error: Option<&str>,
    ) -> CoreResult<()> {
        self.router
            .active_bundle()
            .await?
            .outbox
            .record_attempt(id, status, next_attempt_at, error)
            .await
    }
    async fn recent(&self, limit: u32) -> CoreResult<Vec<OutboxEntry>> {
        self.router
            .active_bundle()
            .await?
            .outbox
            .recent(limit)
            .await
    }
}

/// Dynamic metrics source delegating to the active workspace.
#[derive(Clone, Debug)]
pub struct DynamicMetricsSource {
    router: Arc<DynamicStorageRouter>,
}

impl DynamicMetricsSource {
    /// Creates a new dynamic metrics source.
    #[must_use]
    pub fn new(router: Arc<DynamicStorageRouter>) -> Self {
        Self { router }
    }
}

#[async_trait]
impl MetricsSource for DynamicMetricsSource {
    async fn snapshot(&self) -> CoreResult<MetricsSnapshot> {
        self.router.active_bundle().await?.metrics.snapshot().await
    }
}

/// Dynamic connection store delegating to the active workspace.
#[derive(Clone, Debug)]
pub struct DynamicConnectionStore {
    router: Arc<DynamicStorageRouter>,
}

impl DynamicConnectionStore {
    /// Creates a new dynamic connection store.
    #[must_use]
    pub fn new(router: Arc<DynamicStorageRouter>) -> Self {
        Self { router }
    }
}

#[async_trait]
impl ConnectionStore for DynamicConnectionStore {
    async fn record(&self, record: &ConnectionRecord) -> CoreResult<()> {
        if let Some(conn) = &self.router.active_bundle().await?.connections {
            conn.record(record).await
        } else {
            Ok(())
        }
    }
    async fn get(&self, id: ConnectionId) -> CoreResult<Option<ConnectionRecord>> {
        if let Some(conn) = &self.router.active_bundle().await?.connections {
            conn.get(id).await
        } else {
            Ok(None)
        }
    }
    async fn recent(
        &self,
        protocol: Option<Protocol>,
        limit: u32,
    ) -> CoreResult<Vec<ConnectionRecord>> {
        if let Some(conn) = &self.router.active_bundle().await?.connections {
            conn.recent(protocol, limit).await
        } else {
            Ok(Vec::new())
        }
    }
    async fn prune(&self, keep: u32) -> CoreResult<u64> {
        if let Some(conn) = &self.router.active_bundle().await?.connections {
            conn.prune(keep).await
        } else {
            Ok(0)
        }
    }
}

/// Dynamic readiness probe delegating to the active workspace database.
#[derive(Debug)]
pub struct DynamicReadinessProbe {
    router: Arc<DynamicStorageRouter>,
}

impl DynamicReadinessProbe {
    /// Creates a new dynamic readiness probe.
    #[must_use]
    pub fn new(router: Arc<DynamicStorageRouter>) -> Self {
        Self { router }
    }
}

#[async_trait]
impl ReadinessProbe for DynamicReadinessProbe {
    async fn is_ready(&self) -> bool {
        if let Ok(bundle) = self.router.active_bundle().await {
            sandesh_storage::ping(bundle.database.pool()).await
        } else {
            false
        }
    }
}
