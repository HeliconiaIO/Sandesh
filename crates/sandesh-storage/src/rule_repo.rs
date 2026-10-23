//! [`RuleStore`] over `SQLite` — persisted auto-tagging rules (ADR-087).
//!
//! # Why the JSON is hand-written here
//!
//! `sandesh-core` has no `serde`: its permitted dependencies are fixed at six
//! crates and `tests/architecture.rs` asserts the list. So the domain types
//! carry no derive, and the encoding lives on this side of the port — which is
//! where a storage representation belongs anyway. The core would otherwise be
//! describing its own on-disk shape, and a `PostgreSQL` backend wanting a
//! different one would have to argue with it.
//!
//! Conditions and actions are JSON rather than columns because both are sum
//! types: a condition is one of three shapes, and actions grow in Pass B.
//! Normalising them would mean a table per variant and a join per evaluation,
//! for data read whole, as a set, once per delivery.

use crate::{StorageError, row};
use async_trait::async_trait;
use sandesh_core::{
    CoreResult, Keyword, Rule, RuleAction, RuleCondition, RuleId, RuleStore, Timestamp,
};
use serde_json::{Value, json};
use sqlx::{Row, SqlitePool};

/// `SQLite`-backed rules.
#[derive(Debug)]
pub struct SqliteRuleStore {
    pool: SqlitePool,
}

impl SqliteRuleStore {
    /// Creates a store over `pool`.
    #[must_use]
    pub const fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

/// Encodes a condition as JSON.
fn encode_condition(condition: &RuleCondition) -> Value {
    match condition {
        RuleCondition::HeaderContains { name, value } => json!({
            "kind": "header_contains", "name": name, "value": value,
        }),
        RuleCondition::SenderContains(text) => json!({
            "kind": "sender_contains", "value": text,
        }),
        RuleCondition::RecipientContains(text) => json!({
            "kind": "recipient_contains", "value": text,
        }),
    }
}

/// Decodes one condition, returning `None` for anything unrecognised.
///
/// A row written by a future version with a condition this build does not know
/// is skipped rather than failing the read. The alternative — refusing to load
/// the rule set — would stop every delivery being tagged because of one row,
/// which is a far worse failure than one rule quietly not applying.
fn decode_condition(value: &Value) -> Option<RuleCondition> {
    let text = |key: &str| value.get(key)?.as_str().map(ToOwned::to_owned);

    match value.get("kind")?.as_str()? {
        "header_contains" => Some(RuleCondition::HeaderContains {
            name: text("name")?,
            value: text("value")?,
        }),
        "sender_contains" => Some(RuleCondition::SenderContains(text("value")?)),
        "recipient_contains" => Some(RuleCondition::RecipientContains(text("value")?)),
        _ => None,
    }
}

/// Encodes an action as JSON.
fn encode_action(action: &RuleAction) -> Value {
    match action {
        RuleAction::AddKeyword(keyword) => json!({
            "kind": "add_keyword", "keyword": keyword.as_str(),
        }),
    }
}

/// Decodes an action, returning `None` for anything unrecognised.
fn decode_action(value: &Value) -> Option<RuleAction> {
    match value.get("kind")?.as_str()? {
        "add_keyword" => Keyword::new(value.get("keyword")?.as_str()?).map(RuleAction::AddKeyword),
        _ => None,
    }
}

#[async_trait]
impl RuleStore for SqliteRuleStore {
    async fn save(&self, rule: &Rule) -> CoreResult<()> {
        let conditions = Value::Array(rule.conditions.iter().map(encode_condition).collect());

        sqlx::query(
            "INSERT INTO rules (id, name, conditions, action, enabled, created_at)
             VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET
                name       = excluded.name,
                conditions = excluded.conditions,
                action     = excluded.action,
                enabled    = excluded.enabled",
        )
        .bind(&rule.id.as_bytes()[..])
        .bind(rule.name.clone())
        .bind(conditions.to_string())
        .bind(encode_action(&rule.action).to_string())
        .bind(i64::from(rule.enabled))
        .bind(rule.created_at.as_micros())
        .execute(&self.pool)
        .await
        .map_err(StorageError::from)?;

        Ok(())
    }

    async fn all(&self) -> CoreResult<Vec<Rule>> {
        let rows = sqlx::query("SELECT * FROM rules ORDER BY created_at ASC")
            .fetch_all(&self.pool)
            .await
            .map_err(StorageError::from)?;

        let mut rules = Vec::with_capacity(rows.len());
        for r in &rows {
            let conditions: String = r.try_get("conditions").map_err(StorageError::from)?;
            let action: String = r.try_get("action").map_err(StorageError::from)?;
            let enabled: i64 = r.try_get("enabled").map_err(StorageError::from)?;
            let created_at: i64 = r.try_get("created_at").map_err(StorageError::from)?;

            let conditions: Value = serde_json::from_str(&conditions)
                .map_err(|e| StorageError::corrupt("rules", format!("bad conditions: {e}")))?;
            let action: Value = serde_json::from_str(&action)
                .map_err(|e| StorageError::corrupt("rules", format!("bad action: {e}")))?;

            // An action this build cannot decode means the rule cannot be
            // applied at all, so the rule is skipped — unlike a single unknown
            // condition, which only narrows it.
            let Some(action) = decode_action(&action) else {
                continue;
            };

            rules.push(Rule {
                id: RuleId::from_bytes(row::uuid_bytes(r, "id", "rules")?),
                name: r.try_get("name").map_err(StorageError::from)?,
                conditions: conditions
                    .as_array()
                    .map(|items| items.iter().filter_map(decode_condition).collect())
                    .unwrap_or_default(),
                action,
                enabled: enabled != 0,
                created_at: Timestamp::from_micros(created_at),
            });
        }

        Ok(rules)
    }

    async fn remove(&self, id: RuleId) -> CoreResult<bool> {
        // `delivery_rules` rows are deliberately left alone: they are history,
        // and they copied the name rather than referencing it precisely so a
        // removed rule cannot erase the record of what it did.
        let result = sqlx::query("DELETE FROM rules WHERE id = ?")
            .bind(&id.as_bytes()[..])
            .execute(&self.pool)
            .await
            .map_err(StorageError::from)?;

        Ok(result.rows_affected() > 0)
    }
}
