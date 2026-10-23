//! Credentials and key material must never be renderable (phase brief §11).
//!
//! The leak these guard against is not a careless `println!` — it is a
//! `#[derive(Debug)]` on a struct that holds a password, rendered by `tracing`
//! into a log file that then lives forever. `Secret` makes that impossible by
//! construction (ADR-035); these tests assert the property holds for the types
//! the IMAP session actually stores.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use sandesh_config::{AuthMode, ImapConfig};
use sandesh_imap::{Capabilities, ImapServer};
use sandesh_proto::{Credentials, Secret};

/// The password used throughout, distinctive enough to find in any output.
const PASSWORD: &str = "unmistakeable-plaintext-password-42";

#[test]
fn a_secret_renders_as_redacted_through_debug_and_display() {
    let secret = Secret::new(PASSWORD);

    let debug = format!("{secret:?}");
    let display = format!("{secret}");

    assert!(
        !debug.contains(PASSWORD),
        "Debug leaked the password: {debug}"
    );
    assert!(
        !display.contains(PASSWORD),
        "Display leaked the password: {display}"
    );
}

#[test]
fn a_credential_nested_in_a_struct_still_redacts() {
    // The realistic leak: the password is not logged directly, it is a field of
    // something that is. `Credentials` derives `Debug`, so this must hold.
    let credentials = Credentials {
        username: String::from("alice"),
        password: Secret::new(PASSWORD),
    };

    let rendered = format!("{credentials:?}");
    assert!(
        !rendered.contains(PASSWORD),
        "a nested Secret leaked: {rendered}"
    );
    assert!(
        rendered.contains("alice"),
        "the username is not secret and should still be visible for logs: {rendered}"
    );
}

#[tokio::test]
async fn the_server_debug_reveals_no_key_material() {
    // `TlsAcceptor` holds the private key. A `Debug` that rendered it would put
    // the key in any log line that formatted the server.
    let dir = tempfile::tempdir().expect("temporary directory");
    let identity =
        sandesh_proto::Identity::load_or_generate(dir.path()).expect("generate identity");
    let fingerprint = identity.fingerprint();
    let acceptor = identity.acceptor().expect("build acceptor");

    let config = ImapConfig {
        bind: Some("127.0.0.1:0".parse().expect("valid address")),
        auth_mode: AuthMode::Required,
        start_tls: true,
        ..ImapConfig::default()
    };

    let store: std::sync::Arc<dyn sandesh_core::AccountStore> = {
        // A real store, so the type under test is the production one.
        let db = sandesh_storage::Database::open_in_memory()
            .await
            .expect("open database");
        let events = sandesh_storage::EventRecorder::new(std::sync::Arc::new(
            sandesh_storage::CollectingSink::new(),
        ));
        std::sync::Arc::new(sandesh_storage::SqliteAccountStore::new(
            db.pool().clone(),
            events,
        ))
    };

    let server = ImapServer::new(config, store).with_tls(acceptor);
    let rendered = format!("{server:?}");

    // PEM key material is unmistakeable, as is a raw private-key marker.
    for marker in ["PRIVATE KEY", "BEGIN", "PrivateKey", "-----"] {
        assert!(
            !rendered.contains(marker),
            "the server Debug rendered key material ({marker}): {rendered}"
        );
    }

    // A fingerprint is a public value and is fine to show, but the Debug should
    // report TLS as a boolean rather than dumping the identity.
    if let Some(fingerprint) = fingerprint {
        assert!(
            !rendered.contains(&fingerprint) || rendered.contains("tls: true"),
            "the server Debug should report TLS presence, not the identity: {rendered}"
        );
    }
    assert!(
        rendered.contains("tls: true"),
        "TLS presence should be reported as a boolean: {rendered}"
    );
}

#[test]
fn the_capability_list_never_contains_a_credential() {
    // Belt and braces: the capability line is written to every client, so
    // nothing secret may reach it.
    for encrypted in [false, true] {
        let line = Capabilities::new(AuthMode::Required, true, encrypted).to_line();
        assert!(!line.contains(PASSWORD), "{line}");
        // AUTH= names mechanisms, never credentials.
        assert!(!line.to_lowercase().contains("password"), "{line}");
    }
}
