//! Turning an accepted transaction into stored mail (`CORE_SPEC.md` §7).
//!
//! Routing follows §7 exactly:
//!
//! * envelope recipients decide placement, never `To:`/`Cc:` — which is the
//!   only way `Bcc` testing works;
//! * several recipients produce **one message and several placements**, so the
//!   bytes are stored once;
//! * two recipients resolving to the same mailbox produce one placement;
//! * nothing is ever relayed.
//!
//! ## Why this is not in the SMTP adapter
//!
//! It was, until ADR-071. Nothing in `resolve`, `inbox_of` or the placement
//! deduplication is SMTP-shaped — they are §7's rules — and leaving them in one
//! protocol's crate meant no other surface could reach them: the HTTP injection
//! endpoint could not route a message the way SMTP does without a second
//! implementation.
//!
//! What stays in the adapter is the protocol: the transaction state machine,
//! `RCPT TO` reply codes, and `DATA` framing (`CORE_SPEC.md` §9).

use crate::{
    AccountId, AccountService, AccountStore, ConnectionId, CoreResult, Delivery, DeliveryId,
    DeliveryOutcome, DeliverySource, DeliveryStore, EmailAddress, Envelope, MailboxId,
    MailboxService, MailboxStore, MessageFlags, MessageId, MessageStore, NewMessage, NewPlacement,
    RecipientOutcome, RuleSet, RuleStore, Timestamp, UnknownRecipient, WorkspaceStore,
};
use std::sync::Arc;

/// Turns an accepted SMTP transaction into stored mail.
///
/// Holds the storage ports as trait objects, which is what lets an SMTP test
/// run against in-memory doubles with no database at all.
pub struct DeliveryService {
    accounts: Arc<dyn AccountStore>,
    mailboxes: Arc<dyn MailboxStore>,
    messages: Arc<dyn MessageStore>,
    deliveries: Arc<dyn DeliveryStore>,
    policy: UnknownRecipient,
    /// Auto-tagging rules (ADR-087), when any surface configures them.
    ///
    /// `None` rather than an empty store, so a caller that has no rules at all
    /// — every SMTP test double — costs no query per delivery. With rules
    /// configured, `all()` is read once per delivery inside `deliver`.
    rules: Option<Arc<dyn RuleStore>>,
    /// Workspaces management port, for per-workspace authentication gating.
    workspaces: Option<Arc<dyn WorkspaceStore>>,
}

impl std::fmt::Debug for DeliveryService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeliveryService")
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

/// What a recipient resolved to before the message was stored.
#[derive(Debug, Clone)]
struct Resolved {
    address: EmailAddress,
    account: Option<AccountId>,
    mailbox: Option<MailboxId>,
    outcome: DeliveryOutcome,
    reason: Option<String>,
}

/// The result of delivering one SMTP transaction.
#[derive(Debug, Clone)]
pub struct DeliveryReceipt {
    /// The stored message.
    pub message_id: MessageId,
    /// The delivery record.
    pub delivery_id: DeliveryId,
    /// How many placements were created.
    pub placements: usize,
}

/// Why a recipient cannot be accepted at `RCPT TO` time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecipientRejection {
    /// No local identity matches and the policy is `strict`.
    NoSuchUser,
}

impl DeliveryService {
    /// Builds a service over the storage ports.
    #[must_use]
    pub fn new(
        accounts: Arc<dyn AccountStore>,
        mailboxes: Arc<dyn MailboxStore>,
        messages: Arc<dyn MessageStore>,
        deliveries: Arc<dyn DeliveryStore>,
        policy: UnknownRecipient,
    ) -> Self {
        Self {
            accounts,
            mailboxes,
            messages,
            deliveries,
            policy,
            rules: None,
            workspaces: None,
        }
    }

    /// Attaches the rule store, enabling auto-tagging at delivery (ADR-087).
    ///
    /// Separate from [`new`](Self::new) rather than a sixth parameter: rules
    /// are optional, and four of the five construction sites are test doubles
    /// that have none. A builder keeps those call sites honest about what they
    /// are not exercising, instead of threading `None` through each.
    #[must_use]
    pub fn with_rules(mut self, rules: Arc<dyn RuleStore>) -> Self {
        self.rules = Some(rules);
        self
    }

    /// Attaches the workspace store, enabling per-workspace authentication checks.
    #[must_use]
    pub fn with_workspaces(mut self, workspaces: Arc<dyn WorkspaceStore>) -> Self {
        self.workspaces = Some(workspaces);
        self
    }

    /// Whether the active workspace requires authentication.
    pub async fn is_auth_required(&self) -> bool {
        let Some(ws_store) = &self.workspaces else {
            return false;
        };
        if let Ok(active_id) = ws_store.active_id().await
            && let Ok(Some(ws)) = ws_store.get(&active_id).await
        {
            return ws.auth_required;
        }
        false
    }

    /// Evaluates the configured rules, if any.
    ///
    /// A storage failure reading the rules is **not** allowed to fail the
    /// delivery: mail that arrived must be stored. The delivery simply records
    /// no rules, which is honest — none were applied.
    async fn apply_rules(
        &self,
        parsed: &crate::ParsedMessage,
        sender: Option<&EmailAddress>,
        recipients: &[EmailAddress],
    ) -> Vec<crate::RuleMatch> {
        let Some(store) = &self.rules else {
            return Vec::new();
        };

        match store.all().await {
            Ok(rules) => RuleSet::new(rules).evaluate(parsed, sender, recipients),
            Err(_) => Vec::new(),
        }
    }

    /// Checks whether a recipient can be accepted, at `RCPT TO` time.
    ///
    /// Under `strict` this is where an unknown recipient earns its `550`.
    /// Under the other policies every syntactically usable address is accepted,
    /// and resolution happens at delivery.
    ///
    /// # Errors
    /// The stored Argon2id hash for a username, if the account has one.
    ///
    /// Returns `None` for an unknown account **and** for a known account with
    /// no credential, deliberately conflating them: the caller runs the
    /// verifier either way so the two cases cost the same, and telling them
    /// apart here would only invite a caller to short-circuit and rebuild the
    /// username oracle (ADR-032).
    ///
    /// A storage failure is reported as `Err` rather than folded into `None`:
    /// authentication must fail closed either way, but the adapter needs to
    /// tell "no such account" from "the database could not answer" in order to
    /// log the second. The core does not log — it has no `tracing`
    /// dependency (ARCHITECTURE §3.2) — so it hands the caller what it knows.
    ///
    /// # Errors
    /// [`CoreError::Storage`] when the account could not be read.
    pub async fn credential_hash(&self, username: &str) -> CoreResult<Option<String>> {
        Ok(self
            .accounts
            .get_by_username(username)
            .await?
            .and_then(|account| account.credential)
            .map(|credential| credential.hash))
    }

    /// Whether a recipient may be accepted.
    ///
    /// # Errors
    /// Propagates a storage failure, which the session maps to `451`.
    pub async fn check_recipient(
        &self,
        address: &EmailAddress,
    ) -> CoreResult<Result<(), RecipientRejection>> {
        if self.policy != UnknownRecipient::Strict {
            return Ok(Ok(()));
        }
        let known = self.accounts.resolve_address(address).await?.is_some();
        Ok(if known {
            Ok(())
        } else {
            Err(RecipientRejection::NoSuchUser)
        })
    }

    /// Records a transaction the server refused before storing anything.
    ///
    /// The delivery view has to explain rejections, not only successes — "the
    /// message never arrived" is the single most common thing a developer
    /// needs explained, and a view that only lists what worked cannot answer
    /// it. The record carries `message_id: None`, which the schema permits and
    /// which is what distinguishes it from a delivery: nothing was stored, so
    /// there is nothing to point at.
    ///
    /// Every recipient is marked [`DeliveryOutcome::Rejected`] with the same
    /// reason, because a transaction-level refusal like an oversize message
    /// rejects the transaction rather than any particular address.
    ///
    /// # Errors
    /// Returns a [`CoreError`] if the record could not be written.
    pub async fn record_rejection(
        &self,
        envelope: Envelope,
        recipients: Vec<EmailAddress>,
        reason: String,
        started: Timestamp,
        connection_id: Option<ConnectionId>,
    ) -> CoreResult<DeliveryId> {
        let outcomes = recipients
            .into_iter()
            .map(|address| RecipientOutcome {
                rcpt_to: address,
                resolved_account: None,
                entry_id: None,
                outcome: DeliveryOutcome::Rejected,
                reason: Some(reason.clone()),
            })
            .collect();

        let delivery = Delivery {
            id: DeliveryId::new(),
            message_id: None,
            source: DeliverySource::Smtp,
            envelope,
            outcomes,
            received_at: started,
            duration_us: Timestamp::now().as_micros() - started.as_micros(),
            connection_id,
            // A rejected transaction stored no message, so no rule could have
            // tagged anything. Empty is the true reading, not a placeholder.
            rules_applied: Vec::new(),
            replay_of: None,
        };

        self.deliveries.record(&delivery).await?;
        Ok(delivery.id)
    }

    /// Stores an accepted transaction and records the delivery.
    ///
    /// Raw bytes are parsed *after* they are handed to the store, and a parse
    /// failure never blocks storage: a message that will not parse is exactly
    /// what a developer is often trying to test.
    ///
    /// # Errors
    /// Returns a [`CoreError`] if the message could not be stored. This path
    /// records a delivery that stored a message; a transaction refused before
    /// storage is recorded by [`record_rejection`](Self::record_rejection)
    /// instead, so the two can never be confused for one another.
    pub async fn deliver(&self, request: DeliveryRequest) -> CoreResult<DeliveryReceipt> {
        let started = Timestamp::now();

        // Resolve every recipient before storing anything, so placement is
        // known up front and the whole insert can be one transaction.
        let mut resolved = Vec::with_capacity(request.recipients.len());
        for address in &request.recipients {
            resolved.push(self.resolve(address).await?);
        }

        let raw = crate::RawMessage::new(request.content);
        let parsed = crate::parse(raw.bytes());

        // Rules are evaluated before placements are built, so their keywords
        // are part of the same insert rather than a second write that could
        // fail on its own (ADR-087) — the rule ADR-074 set for the search index
        // and ADR-086 for the outbox: the fact and the record of it commit
        // together.
        let rules_applied = self
            .apply_rules(&parsed, request.mail_from.as_ref(), &request.recipients)
            .await;

        let mut tagged = MessageFlags::empty();
        tagged.add(
            crate::SystemFlags::NONE,
            rules_applied.iter().map(|m| m.keyword.clone()),
        );

        // Deduplicate placements: two recipients landing in one mailbox produce
        // one entry (CORE_SPEC §7.3).
        let mut placements: Vec<NewPlacement> = Vec::new();
        for target in resolved.iter().filter_map(|r| r.mailbox) {
            if !placements.iter().any(|p| p.mailbox_id == target) {
                placements.push(NewPlacement {
                    mailbox_id: target,
                    // Delivered mail starts unread and unflagged, plus whatever
                    // the rules added. Every placement gets the same keywords:
                    // a rule matched the *message*, and tagging only one of its
                    // mailboxes would make the tag depend on delivery order.
                    flags: tagged.clone(),
                    internal_date: started,
                });
            }
        }

        let envelope = Envelope {
            mail_from: request.mail_from.clone(),
            rcpt_to: request.recipients.clone(),
            helo_domain: request.helo.clone(),
            peer: request.peer.clone(),
            tls: None,
            auth_identity: request.auth_identity.clone(),
        };

        let inserted = self
            .messages
            .insert(&NewMessage {
                raw,
                envelope: envelope.clone(),
                source: DeliverySource::Smtp,
                parsed,
                received_at: started,
                placements,
            })
            .await?;

        // Pair each recipient with the placement it produced, so the delivery
        // record answers "why did this land here?" per address.
        let outcomes = resolved
            .iter()
            .map(|r| RecipientOutcome {
                rcpt_to: r.address.clone(),
                resolved_account: r.account,
                entry_id: r.mailbox.and_then(|mailbox| {
                    inserted
                        .entries
                        .iter()
                        .find(|e| e.mailbox_id == mailbox)
                        .map(|e| e.id)
                }),
                outcome: r.outcome,
                reason: r.reason.clone(),
            })
            .collect();

        let delivery = Delivery {
            id: DeliveryId::new(),
            message_id: Some(inserted.message_id),
            source: DeliverySource::Smtp,
            envelope,
            outcomes,
            received_at: started,
            duration_us: Timestamp::now().as_micros() - started.as_micros(),
            connection_id: request.connection_id,
            rules_applied,
            replay_of: None,
        };
        self.deliveries.record(&delivery).await?;

        Ok(DeliveryReceipt {
            message_id: inserted.message_id,
            delivery_id: delivery.id,
            placements: inserted.entries.len(),
        })
    }

    /// Resolves one envelope recipient to a mailbox, per the configured policy.
    async fn resolve(&self, address: &EmailAddress) -> CoreResult<Resolved> {
        if let Some(account) = self.accounts.resolve_address(address).await? {
            let mailbox = self.inbox_of(account).await?;
            return Ok(Resolved {
                address: address.clone(),
                account: Some(account),
                mailbox: Some(mailbox),
                outcome: DeliveryOutcome::Delivered,
                reason: None,
            });
        }

        match self.policy {
            UnknownRecipient::CatchAll => match self.accounts.catch_all().await? {
                Some(account) => Ok(Resolved {
                    address: address.clone(),
                    account: Some(account.id),
                    mailbox: Some(self.inbox_of(account.id).await?),
                    outcome: DeliveryOutcome::CatchAll,
                    reason: Some(format!(
                        "no identity matches {}; routed to the catch-all account",
                        address.normalized()
                    )),
                }),
                // No catch-all configured. The message is still accepted and
                // stored — it simply lands nowhere — because silently
                // discarding mail a server said `250` to would be worse.
                None => Ok(Resolved {
                    address: address.clone(),
                    account: None,
                    mailbox: None,
                    outcome: DeliveryOutcome::Rejected,
                    reason: Some(String::from(
                        "no identity matches and no catch-all account is configured",
                    )),
                }),
            },

            UnknownRecipient::AutoCreate => {
                let account = self.create_account_for(address).await?;
                Ok(Resolved {
                    address: address.clone(),
                    account: Some(account),
                    mailbox: Some(self.inbox_of(account).await?),
                    outcome: DeliveryOutcome::Delivered,
                    reason: Some(String::from("account created on demand")),
                })
            }

            // Already refused at RCPT TO; reaching here means the policy
            // changed mid-transaction. Record it rather than place it.
            UnknownRecipient::Strict => Ok(Resolved {
                address: address.clone(),
                account: None,
                mailbox: None,
                outcome: DeliveryOutcome::Rejected,
                reason: Some(String::from("no such user")),
            }),
        }
    }

    /// The `INBOX` of an account.
    ///
    /// Every account is created with one (`CORE_SPEC.md` §4.4), so its absence
    /// is a broken invariant rather than an ordinary miss.
    async fn inbox_of(&self, account: AccountId) -> CoreResult<MailboxId> {
        MailboxService::new(Arc::clone(&self.mailboxes))
            .inbox_of(account)
            .await
    }

    /// Creates an account for an address, under the `auto_create` policy.
    async fn create_account_for(&self, address: &EmailAddress) -> CoreResult<AccountId> {
        AccountService::new(Arc::clone(&self.accounts))
            .create_for_address(address)
            .await
    }
}

/// An accepted SMTP transaction, ready to store.
#[derive(Debug, Clone)]
pub struct DeliveryRequest {
    /// `MAIL FROM`. `None` is the null reverse-path.
    pub mail_from: Option<EmailAddress>,
    /// Every accepted `RCPT TO`, in order and without deduplication.
    pub recipients: Vec<EmailAddress>,
    /// The message bytes, with SMTP framing already removed.
    pub content: Vec<u8>,
    /// The `EHLO`/`HELO` domain the client announced.
    pub helo: Option<String>,
    /// The peer address.
    pub peer: Option<String>,
    /// The connection this transaction arrived on (ADR-066), so the delivery
    /// record can link to its transcript.
    pub connection_id: Option<ConnectionId>,
    /// The authenticated identity, if the session authenticated before sending.
    pub auth_identity: Option<String>,
}
