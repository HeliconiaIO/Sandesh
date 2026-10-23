//! Integration tests for workspace management and data purging.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use sandesh_core::{Account, AccountStore, NullEventSink, PurgeOptions, Timestamp};
use sandesh_storage::{EventRecorder, SqliteAccountStore, WorkspaceManager};
use std::sync::Arc;
use tempfile::TempDir;

#[tokio::test]
async fn workspace_lifecycle_and_switching() {
    let dir = TempDir::new().expect("tempdir");
    let data_dir = dir.path().to_path_buf();
    let default_db = data_dir.join("sandesh.db");
    let default_blobs = data_dir.join("sandesh-blobs");

    let recorder = EventRecorder::new(Arc::new(NullEventSink));
    let mgr = WorkspaceManager::load_or_init(
        data_dir.clone(),
        default_db.clone(),
        default_blobs.clone(),
        recorder.clone(),
    )
    .await
    .expect("init workspace manager");

    // Initially, default workspace exists and is active
    let list = mgr.list().await.expect("list workspaces");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, "default");
    assert!(list[0].is_active);
    assert_eq!(mgr.active_id().await, "default");

    // Create a new workspace: "billing-service"
    let created = mgr
        .create(
            "billing-service",
            "Billing Service",
            Some("Invoice tests"),
            None,
            None,
            false,
        )
        .await
        .expect("create workspace");

    assert_eq!(created.id, "billing-service");
    assert_eq!(created.name, "Billing Service");
    assert!(!created.is_active);

    let list_after = mgr.list().await.expect("list workspaces");
    assert_eq!(list_after.len(), 2);

    // Switch active workspace to "billing-service"
    mgr.set_active("billing-service").await.expect("set active");
    assert_eq!(mgr.active_id().await, "billing-service");

    let active_ws = mgr
        .get("billing-service")
        .await
        .expect("get")
        .expect("exists");
    assert!(active_ws.is_active);

    // Default workspace should no longer be active
    let default_ws = mgr.get("default").await.expect("get").expect("exists");
    assert!(!default_ws.is_active);

    // Switch back
    mgr.set_active("default").await.expect("set active");
    assert_eq!(mgr.active_id().await, "default");
}

#[tokio::test]
async fn cannot_delete_default_workspace() {
    let dir = TempDir::new().expect("tempdir");
    let data_dir = dir.path().to_path_buf();
    let default_db = data_dir.join("sandesh.db");
    let default_blobs = data_dir.join("sandesh-blobs");

    let recorder = EventRecorder::new(Arc::new(NullEventSink));
    let mgr = WorkspaceManager::load_or_init(data_dir, default_db, default_blobs, recorder)
        .await
        .expect("init");

    let err = mgr.delete("default").await;
    assert!(err.is_err(), "default workspace must not be deletable");
}

#[tokio::test]
async fn workspace_deletion_removes_files_from_disk() {
    let dir = TempDir::new().expect("tempdir");
    let data_dir = dir.path().to_path_buf();
    let default_db = data_dir.join("sandesh.db");
    let default_blobs = data_dir.join("sandesh-blobs");

    let recorder = EventRecorder::new(Arc::new(NullEventSink));
    let mgr = WorkspaceManager::load_or_init(data_dir.clone(), default_db, default_blobs, recorder)
        .await
        .expect("init");

    let _ = mgr
        .create("project-x", "Project X", None, None, None, false)
        .await
        .expect("create");

    let ws_dir = data_dir.join("workspaces").join("project-x");
    assert!(ws_dir.exists());

    let deleted = mgr.delete("project-x").await.expect("delete");
    assert!(deleted);
    assert!(
        !ws_dir.exists(),
        "workspace folder should be removed from disk"
    );

    let list = mgr.list().await.expect("list");
    assert_eq!(list.len(), 1);
}

#[tokio::test]
async fn workspace_purge_clears_data_and_resets_counters() {
    let dir = TempDir::new().expect("tempdir");
    let data_dir = dir.path().to_path_buf();
    let default_db = data_dir.join("sandesh.db");
    let default_blobs = data_dir.join("sandesh-blobs");

    let recorder = EventRecorder::new(Arc::new(NullEventSink));
    let mgr = WorkspaceManager::load_or_init(
        data_dir.clone(),
        default_db.clone(),
        default_blobs.clone(),
        recorder.clone(),
    )
    .await
    .expect("init");

    let ws = mgr
        .create("purge-test", "Purge Test", None, None, None, false)
        .await
        .expect("create");

    let db = mgr.get_or_open_db(&ws.id).await.expect("open db");

    // Insert dummy account and mailbox
    let account = Account {
        id: sandesh_core::AccountId::new(),
        username: "test-user".to_string(),
        credential: None,
        is_catch_all: false,
        created_at: Timestamp::now(),
    };
    let account_store = SqliteAccountStore::new(db.pool().clone(), recorder.clone());
    account_store
        .create(&account)
        .await
        .expect("create account");

    // Purge data
    let res = mgr
        .purge(&ws.id, PurgeOptions::default())
        .await
        .expect("purge");

    assert_eq!(res.workspace_id, "purge-test");

    // Accounts survive mail-only purge
    let accounts = account_store.list().await.expect("list accounts");
    assert!(!accounts.is_empty());
}

#[tokio::test]
async fn workspace_clone_accounts_and_rules() {
    let dir = TempDir::new().expect("tempdir");
    let data_dir = dir.path().to_path_buf();
    let default_db = data_dir.join("sandesh.db");
    let default_blobs = data_dir.join("sandesh-blobs");

    let recorder = EventRecorder::new(Arc::new(NullEventSink));
    let mgr = WorkspaceManager::load_or_init(
        data_dir.clone(),
        default_db.clone(),
        default_blobs.clone(),
        recorder.clone(),
    )
    .await
    .expect("init");

    // Add a custom account and email identity to default workspace
    let def_db = mgr
        .get_or_open_db("default")
        .await
        .expect("open default db");
    let account = Account {
        id: sandesh_core::AccountId::new(),
        username: "invoice-bot".to_string(),
        credential: None,
        is_catch_all: false,
        created_at: Timestamp::now(),
    };
    let account_store = SqliteAccountStore::new(def_db.pool().clone(), recorder.clone());
    account_store
        .create(&account)
        .await
        .expect("create invoice-bot");

    let invoice_addr = sandesh_core::EmailAddress::parse_lenient("invoices@example.com");
    account_store
        .add_identity(account.id, &invoice_addr, true)
        .await
        .expect("add identity");

    // Create a new workspace cloning from "default"
    let cloned_ws = mgr
        .create(
            "cloned-project",
            "Cloned Project",
            None,
            Some("default"),
            None,
            false,
        )
        .await
        .expect("create cloned workspace");

    assert_eq!(cloned_ws.id, "cloned-project");
    assert_eq!(cloned_ws.message_count, 0);

    // Verify the new workspace contains the cloned account and identity
    let cloned_db = mgr
        .get_or_open_db("cloned-project")
        .await
        .expect("open cloned db");
    let cloned_acc_store = SqliteAccountStore::new(cloned_db.pool().clone(), recorder.clone());
    let accounts = cloned_acc_store.list().await.expect("list accounts");
    assert!(accounts.iter().any(|a| a.username == "invoice-bot"));

    let resolved = cloned_acc_store
        .resolve_address(&sandesh_core::EmailAddress::parse_lenient(
            "invoices@example.com",
        ))
        .await
        .expect("resolve address");
    assert_eq!(resolved, Some(account.id));
}

#[tokio::test]
async fn workspace_creation_with_custom_initial_account_and_bound_addresses() {
    let dir = TempDir::new().expect("tempdir");
    let data_dir = dir.path().to_path_buf();
    let default_db = data_dir.join("sandesh.db");
    let default_blobs = data_dir.join("sandesh-blobs");

    let recorder = EventRecorder::new(Arc::new(NullEventSink));
    let mgr = WorkspaceManager::load_or_init(
        data_dir.clone(),
        default_db.clone(),
        default_blobs.clone(),
        recorder.clone(),
    )
    .await
    .expect("init workspace manager");

    let initial = sandesh_core::InitialAccountSetup {
        username: "billing".to_string(),
        addresses: vec![
            "billing@example.com".to_string(),
            "invoices@example.com".to_string(),
        ],
        is_catch_all: false,
        password_hash: Some("$argon2id$v=19$test$hash".to_string()),
    };

    let ws = mgr
        .create(
            "billing-corp",
            "Billing Corp",
            Some("Dedicated billing department"),
            None,
            Some(initial),
            true,
        )
        .await
        .expect("create workspace with initial account");

    assert_eq!(ws.id, "billing-corp");
    assert!(ws.auth_required);

    let db = mgr.get_or_open_db("billing-corp").await.expect("open db");
    let acc_store = SqliteAccountStore::new(db.pool().clone(), recorder.clone());

    let accounts = acc_store.list().await.expect("list accounts");
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0].username, "billing");
    assert!(!accounts[0].is_catch_all);
    assert!(accounts[0].credential.is_some());
    assert_eq!(
        accounts[0].credential.as_ref().unwrap().hash,
        "$argon2id$v=19$test$hash"
    );

    // Verify bound addresses resolve to this account
    let billing_resolved = acc_store
        .resolve_address(&sandesh_core::EmailAddress::parse_lenient(
            "billing@example.com",
        ))
        .await
        .expect("resolve billing address");
    assert_eq!(billing_resolved, Some(accounts[0].id));

    let invoices_resolved = acc_store
        .resolve_address(&sandesh_core::EmailAddress::parse_lenient(
            "invoices@example.com",
        ))
        .await
        .expect("resolve invoices address");
    assert_eq!(invoices_resolved, Some(accounts[0].id));

    // Test updating workspace metadata and auth requirement toggle
    let updated = mgr
        .update(
            "billing-corp",
            sandesh_core::WorkspaceUpdate {
                name: Some("Billing & Payments".to_string()),
                description: Some(Some("Updated description".to_string())),
                auth_required: Some(false),
            },
        )
        .await
        .expect("update workspace");

    assert_eq!(updated.name, "Billing & Payments");
    assert_eq!(updated.description.as_deref(), Some("Updated description"));
    assert!(!updated.auth_required);

    // Verify persisted in get()
    let fetched = mgr.get("billing-corp").await.expect("get").expect("exists");
    assert_eq!(fetched.name, "Billing & Payments");
    assert!(!fetched.auth_required);
}
