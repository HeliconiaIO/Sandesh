//! Auto-tagging rules and the delivery record that explains them (ADR-087).
//!
//! The assertion that matters is the *pair*: a keyword on the message and a
//! row saying which rule put it there. Either alone would pass a weaker test
//! and miss the point of the increment — a tag with no explanation is the
//! opacity Phase 7 was built to remove, and an explanation with no tag is a
//! record of something that did not happen.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::Harness;
use sandesh_core::{
    Delivery, DeliveryId, DeliveryOutcome, DeliverySource, DeliveryStore, EmailAddress, Envelope,
    Keyword, RecipientOutcome, Rule, RuleAction, RuleCondition, RuleId, RuleMatch, RuleSet,
    RuleStore, Timestamp,
};
use sandesh_storage::{EventRecorder, SqliteDeliveryStore, SqliteRuleStore};

const INVOICE: &[u8] = b"From: billing@vendor.test\r\n\
                          To: dev@example.test\r\n\
                          Subject: Invoice 4021\r\n\
                          \r\n\
                          body\r\n";

fn keyword(text: &str) -> Keyword {
    Keyword::new(text).expect("a valid keyword")
}

fn rules(harness: &Harness) -> SqliteRuleStore {
    SqliteRuleStore::new(harness.db.pool().clone())
}

fn deliveries(harness: &Harness) -> SqliteDeliveryStore {
    SqliteDeliveryStore::new(
        harness.db.pool().clone(),
        EventRecorder::new(harness.sink.clone()),
    )
}

fn invoice_rule() -> Rule {
    Rule {
        id: RuleId::new(),
        name: String::from("invoices"),
        conditions: vec![RuleCondition::HeaderContains {
            name: String::from("subject"),
            value: String::from("invoice"),
        }],
        action: RuleAction::AddKeyword(keyword("$Invoice")),
        enabled: true,
        created_at: Timestamp::now(),
    }
}

#[tokio::test]
async fn a_rule_round_trips_through_storage() {
    let harness = Harness::new().await;
    let store = rules(&harness);
    let rule = invoice_rule();

    store.save(&rule).await.unwrap();

    let loaded = store.all().await.unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0], rule, "a rule must survive encoding unchanged");
}

#[tokio::test]
async fn every_condition_shape_survives_the_round_trip() {
    // The JSON encoding is hand-written (the core has no serde), so each
    // variant needs asserting rather than trusting a derive.
    let harness = Harness::new().await;
    let store = rules(&harness);

    let mut rule = invoice_rule();
    rule.conditions = vec![
        RuleCondition::HeaderContains {
            name: String::from("x-spam-flag"),
            value: String::from("yes"),
        },
        RuleCondition::SenderContains(String::from("billing@")),
        RuleCondition::RecipientContains(String::from("support@")),
    ];
    store.save(&rule).await.unwrap();

    assert_eq!(store.all().await.unwrap()[0].conditions, rule.conditions);
}

#[tokio::test]
async fn saving_the_same_id_updates_rather_than_duplicating() {
    let harness = Harness::new().await;
    let store = rules(&harness);

    let mut rule = invoice_rule();
    store.save(&rule).await.unwrap();

    rule.name = String::from("renamed");
    rule.enabled = false;
    store.save(&rule).await.unwrap();

    let loaded = store.all().await.unwrap();
    assert_eq!(loaded.len(), 1, "an upsert, not an insert");
    assert_eq!(loaded[0].name, "renamed");
    assert!(!loaded[0].enabled);
}

#[tokio::test]
async fn removing_a_rule_reports_whether_one_was_there() {
    let harness = Harness::new().await;
    let store = rules(&harness);
    let rule = invoice_rule();
    store.save(&rule).await.unwrap();

    assert!(store.remove(rule.id).await.unwrap());
    assert!(store.all().await.unwrap().is_empty());
    assert!(!store.remove(rule.id).await.unwrap(), "already gone");
}

#[tokio::test]
async fn a_delivery_records_which_rule_fired_and_what_it_did() {
    let harness = Harness::new().await;
    let store = deliveries(&harness);
    let rule = invoice_rule();

    let delivery = Delivery {
        id: DeliveryId::new(),
        message_id: None,
        source: DeliverySource::Smtp,
        envelope: Envelope::default(),
        outcomes: vec![RecipientOutcome {
            rcpt_to: EmailAddress::parse_lenient("dev@example.test"),
            resolved_account: None,
            entry_id: None,
            outcome: DeliveryOutcome::Delivered,
            reason: None,
        }],
        received_at: Timestamp::now(),
        duration_us: 1,
        connection_id: None,
        replay_of: None,
        rules_applied: vec![RuleMatch {
            rule_id: rule.id,
            rule_name: rule.name.clone(),
            keyword: keyword("$Invoice"),
        }],
    };

    store.record(&delivery).await.unwrap();

    let loaded = store.get(delivery.id).await.unwrap().expect("recorded");
    assert_eq!(loaded.rules_applied.len(), 1);
    assert_eq!(loaded.rules_applied[0].rule_name, "invoices");
    assert_eq!(loaded.rules_applied[0].keyword.as_str(), "$Invoice");
}

#[tokio::test]
async fn a_deleted_rule_does_not_erase_the_record_of_what_it_did() {
    // The reason `delivery_rules` copies the name instead of referencing it.
    // A record that renders as "rule 019a… matched" once its rule is gone
    // explains nothing, which is the failure this increment exists to avoid.
    let harness = Harness::new().await;
    let rule_store = rules(&harness);
    let delivery_store = deliveries(&harness);

    let rule = invoice_rule();
    rule_store.save(&rule).await.unwrap();

    let delivery = Delivery {
        id: DeliveryId::new(),
        message_id: None,
        source: DeliverySource::Smtp,
        envelope: Envelope::default(),
        outcomes: Vec::new(),
        received_at: Timestamp::now(),
        duration_us: 1,
        connection_id: None,
        replay_of: None,
        rules_applied: vec![RuleMatch {
            rule_id: rule.id,
            rule_name: rule.name.clone(),
            keyword: keyword("$Invoice"),
        }],
    };
    delivery_store.record(&delivery).await.unwrap();

    rule_store.remove(rule.id).await.unwrap();

    let loaded = delivery_store
        .get(delivery.id)
        .await
        .unwrap()
        .expect("kept");
    assert_eq!(
        loaded.rules_applied[0].rule_name, "invoices",
        "history must survive the rule"
    );
}

#[tokio::test]
async fn a_delivery_with_no_rules_records_none() {
    let harness = Harness::new().await;
    let store = deliveries(&harness);

    let delivery = Delivery {
        id: DeliveryId::new(),
        message_id: None,
        source: DeliverySource::Smtp,
        envelope: Envelope::default(),
        outcomes: Vec::new(),
        received_at: Timestamp::now(),
        duration_us: 1,
        connection_id: None,
        rules_applied: Vec::new(),
        replay_of: None,
    };
    store.record(&delivery).await.unwrap();

    assert!(
        store
            .get(delivery.id)
            .await
            .unwrap()
            .expect("recorded")
            .rules_applied
            .is_empty()
    );
}

#[tokio::test]
async fn a_stored_rule_evaluates_against_a_real_message() {
    // The join that matters: rules loaded from storage, evaluated against a
    // message parsed from real bytes. Either half alone could pass while the
    // encoding silently dropped a condition.
    let harness = Harness::new().await;
    let store = rules(&harness);
    store.save(&invoice_rule()).await.unwrap();

    let loaded = RuleSet::new(store.all().await.unwrap());
    let parsed = sandesh_core::parse(INVOICE);

    let matches = loaded.evaluate(&parsed, None, &[]);
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].keyword.as_str(), "$Invoice");
}
