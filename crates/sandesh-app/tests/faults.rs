//! Fault injection across the assembled application (Phase 8 A1, ADR-084).
//!
//! ```text
//! POST /api/v1/faults ──▶ registry ──▶ running SMTP listener ──▶ client sees 554
//!         │                                                          │
//!         └──────────── GET /info reports faults_active ◀─────────────┘
//! ```
//!
//! The unit and `sandesh-smtp` suites prove the registry works and that a
//! session obeys it. Neither proves the **API and the SMTP server share one
//! registry** — with two, every endpoint would return success and change
//! nothing. That is the specific failure this file exists to catch, so every
//! test here configures faults over HTTP and observes them over SMTP.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::{SmtpClient, TestApp};

const MESSAGE: &str = "From: app@test.local\r\n\
                       To: dev@example.test\r\n\
                       Subject: fault injection\r\n\
                       \r\n\
                       body";

/// Configures a fault through the API, asserting it was accepted.
async fn set_fault(app: &TestApp, body: &str) {
    let (status, _) = app
        .request("POST", "/faults", Some(body))
        .await
        .expect("POST /faults");
    assert_eq!(status, 201, "the fault should have been configured");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fault_set_over_http_is_observed_over_smtp() {
    // The load-bearing test: two registries would pass every other assertion
    // in this file's siblings and fail precisely here.
    let app = TestApp::start().await;
    app.await_ready().await;

    // Delivery works before any fault is configured.
    let before = app
        .deliver("app@test.local", "dev@example.test", MESSAGE)
        .await;
    assert!(before.starts_with("250"), "clean delivery: {before}");

    set_fault(&app, r#"{"mode":"reject_at_data"}"#).await;

    let after = app
        .deliver("app@test.local", "dev@example.test", MESSAGE)
        .await;
    assert!(
        after.starts_with("554"),
        "the API-configured fault must reach the running SMTP server: {after}"
    );

    app.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn info_reports_whether_faults_are_active() {
    // The "loudly visible" half of ADR-084: an injected failure must never be
    // invisible to someone wondering why delivery stopped working.
    let app = TestApp::start().await;
    app.await_ready().await;

    let (status, info) = app.get_json("/info").await;
    assert_eq!(status, 200);
    assert_eq!(
        info["faults_active"], false,
        "no fault is configured at startup"
    );

    set_fault(&app, r#"{"mode":"reject_at_rcpt"}"#).await;

    let (_, info) = app.get_json("/info").await;
    assert_eq!(
        info["faults_active"], true,
        "/info must report a configured fault"
    );

    app.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn faults_are_listed_and_cleared_through_the_api() {
    let app = TestApp::start().await;
    app.await_ready().await;

    let (status, listed) = app.get_json("/faults").await;
    assert_eq!(status, 200);
    assert_eq!(listed["active"], false);
    assert_eq!(
        listed["rules"].as_array().expect("rules array").len(),
        0,
        "nothing is configured at startup"
    );

    set_fault(&app, r#"{"mode":"reject_at_rcpt"}"#).await;
    set_fault(
        &app,
        r#"{"mode":"slow_response","delay_ms":10,"selector":{"sender":"slow@test.local"}}"#,
    )
    .await;

    let (_, listed) = app.get_json("/faults").await;
    assert_eq!(listed["active"], true);
    let rules = listed["rules"].as_array().expect("rules array");
    assert_eq!(rules.len(), 2, "both rules should be listed");
    assert_eq!(rules[0]["mode"], "reject_at_rcpt");
    assert_eq!(rules[1]["mode"], "slow_response");
    assert_eq!(
        rules[1]["delay_ms"], 10,
        "the delay must survive the round trip"
    );
    assert_eq!(rules[1]["selector"]["sender"], "slow@test.local");

    let (status, _) = app
        .request("DELETE", "/faults", None)
        .await
        .expect("DELETE /faults");
    assert_eq!(status, 200);

    let (_, listed) = app.get_json("/faults").await;
    assert_eq!(listed["active"], false);
    assert_eq!(listed["rules"].as_array().expect("rules array").len(), 0);

    app.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn clearing_over_http_restores_delivery() {
    // The round trip that matters for a CI suite: fail a scenario, then clear
    // and carry on, in one process.
    let app = TestApp::start().await;
    app.await_ready().await;

    set_fault(&app, r#"{"mode":"reject_at_data"}"#).await;
    let refused = app
        .deliver("app@test.local", "dev@example.test", MESSAGE)
        .await;
    assert!(refused.starts_with("554"), "fault applied: {refused}");

    let (status, _) = app
        .request("DELETE", "/faults", None)
        .await
        .expect("DELETE /faults");
    assert_eq!(status, 200);

    let delivered = app
        .deliver("app@test.local", "dev@example.test", MESSAGE)
        .await;
    assert!(
        delivered.starts_with("250"),
        "clearing must restore delivery with no restart: {delivered}"
    );

    app.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rcpt_fault_is_seen_at_rcpt_not_at_data() {
    // Driven with a raw client because the `deliver` helper asserts 250 at
    // RCPT — which is exactly the reply this fault replaces.
    let app = TestApp::start().await;
    app.await_ready().await;

    set_fault(&app, r#"{"mode":"reject_at_rcpt"}"#).await;

    let mut client = SmtpClient::connect(app.smtp()).await;
    client.expect("220").await;
    client.send("EHLO test.local").await;
    client.read_multiline().await;
    client.send("MAIL FROM:<app@test.local>").await;
    client.expect("250").await;
    client.send("RCPT TO:<dev@example.test>").await;

    let reply = client.read_line().await;
    assert!(
        reply.starts_with("550"),
        "the fault must fire at RCPT: {reply}"
    );

    client.send("QUIT").await;
    app.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_selector_confines_the_fault_to_the_named_sender() {
    let app = TestApp::start().await;
    app.await_ready().await;

    set_fault(
        &app,
        r#"{"mode":"reject_at_data","selector":{"sender":"blocked@test.local"}}"#,
    )
    .await;

    let refused = app
        .deliver("blocked@test.local", "dev@example.test", MESSAGE)
        .await;
    assert!(
        refused.starts_with("554"),
        "named sender refused: {refused}"
    );

    let delivered = app
        .deliver("allowed@test.local", "dev@example.test", MESSAGE)
        .await;
    assert!(
        delivered.starts_with("250"),
        "an unnamed sender must be unaffected: {delivered}"
    );

    app.stop().await;
}
