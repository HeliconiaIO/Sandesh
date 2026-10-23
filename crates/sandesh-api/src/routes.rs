//! REST handlers (`API_DESIGN.md` §3).
//!
//! Every handler does the same three things and nothing else: validate the
//! request, call a domain port, translate the result. There is no SQL here, no
//! entity construction that bypasses an invariant, and no decision that belongs
//! to the core. When a handler looks like it is making a mail-domain judgement,
//! that logic is in the wrong crate.
//!
//! ## Scope
//!
//! Phase 3 implements the read surface the future UI needs plus flag mutation:
//! health, readiness, info, accounts, mailboxes, entries, messages, raw bytes,
//! headers and MIME. The write-heavy endpoints in `API_DESIGN.md` §3 (message
//! injection, copy/move, bulk operations, account and mailbox mutation) are
//! deliberately not implemented here — several depend on core operations that
//! do not exist until IMAP, and implementing them against a missing service
//! would mean putting the logic in the HTTP layer, which §57 forbids.

use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use sandesh_core::{
    AccountId, AttachmentId, Credential, Delivery, DeliveryId, DeliveryOutcome, DeliverySource,
    EmailAddress, EntryId, Envelope, FlagUpdate, INBOX, Keyword, MailboxAttributes, MailboxId,
    MailboxName, Message, MessageEntry, MessageFlags, MessageId, MimePart, NewAccount, NewMessage,
    NewPlacement, RawMessage, RecipientOutcome, SystemFlags, Timestamp, parse_filter_query,
};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use utoipa::{IntoParams, ToSchema};

use crate::dto::{
    AccountDto, AttachmentDto, BulkAction, BulkEntriesRequest, BulkEntriesResponse, ConnectionDto,
    CopyEntryRequest, CreateAccountRequest, CreateMailboxRequest, CreateWorkspaceRequest,
    CreatedWebhookDto, DeliveryDto, DiagnosticDto, EntryDto, FaultRuleDto, FaultsDto,
    HeaderAnalysisDto, HeaderDto, HealthDto, InfoDto, LinkReportDto, ListResponse,
    ListResponseAccountDto, ListResponseAttachmentDto, ListResponseConnectionDto,
    ListResponseDeliveryDto, ListResponseEntryDto, ListResponseMailboxDto,
    ListResponseMessageSummary, LogLineDto, MailboxDto, MessageDetail, MessageSummary, MimePartDto,
    MimeTree, MoveEntryRequest, NewRuleDto, NewWebhookDto, OutboxEntryDto, PatchAccountRequest,
    PatchMailboxRequest, PurgeResultDto, PurgeWorkspaceRequest, ReadyDto, RecipientOutcomeDto,
    RuleDto, SearchExplainResponse, SendMessageRequest, SendMessageResponse,
    UpdateWorkspaceRequest, WebhookEndpointDto, WorkspaceDto, rfc3339,
};
use crate::error::{ApiError, ApiResult};
use crate::state::ApiState;

/// The crate version, reported by `/health` and `/info`.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Default page size (`API_DESIGN.md` §2).
pub const DEFAULT_LIMIT: u32 = 50;

/// Largest page a client may ask for.
///
/// A cap rather than a suggestion: without one, `?limit=100000000` is a request
/// to serialise the entire database into memory, which is a denial of service
/// with a single GET.
pub const MAX_LIMIT: u32 = 500;

/// The most log lines one request may return.
///
/// Matched to the ring's own capacity: asking for more than the buffer holds
/// cannot return more, so a larger cap would only promise what it cannot give.
pub const MAX_LOG_LIMIT: usize = sandesh_events::DEFAULT_LOG_CAPACITY;

/// How the root MIME part is spelled in a URL.
///
/// The domain's own rendering of the root path is the empty string, which
/// cannot appear as a URL path segment.
pub const ROOT_PART_PATH: &str = "root";

// ---------------------------------------------------------------------------
// Health, readiness, info
// ---------------------------------------------------------------------------

/// `GET /api/v1/health` — liveness.
///
/// Touches no dependency. If the process can route a request it is alive, and
/// making liveness query the database turns a slow disk into a restart loop.
#[utoipa::path(
    get, path = "/api/v1/health", tag = "health",
    responses((status = 200, description = "The process is alive", body = HealthDto))
)]
pub async fn health() -> Json<HealthDto> {
    Json(HealthDto {
        status: "ok",
        version: VERSION,
    })
}

/// `GET /api/v1/health/ready` — readiness.
///
/// Unlike liveness this does check the database, and answers `503` when it is
/// unreachable. Reporting ready while storage is down would be a lie that costs
/// a caller a confusing failure later.
#[utoipa::path(
    get, path = "/api/v1/health/ready", tag = "health",
    responses(
        (status = 200, description = "Storage answered", body = ReadyDto),
        (status = 503, description = "Storage is unreachable", body = ReadyDto),
    )
)]
pub async fn ready(State(state): State<ApiState>) -> Response {
    let database = state.readiness.is_ready().await;
    let body = Json(ReadyDto {
        status: if database { "ready" } else { "not_ready" },
        database,
    });

    if database {
        (StatusCode::OK, body).into_response()
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, body).into_response()
    }
}

/// `GET /api/v1/info` — version and connection details.
#[utoipa::path(
    get, path = "/api/v1/info", tag = "health",
    responses((status = 200, description = "Version and connection details", body = InfoDto))
)]
pub async fn info(State(state): State<ApiState>) -> Json<InfoDto> {
    Json(InfoDto {
        version: VERSION,
        smtp_address: state.info.smtp_address.clone(),
        imap_address: state.info.imap_address.clone(),
        pop3_address: state.info.pop3_address.clone(),
        http_address: state.info.http_address.clone(),
        websocket_path: crate::WEBSOCKET_PATH,
        database: state.info.database.clone(),
        smtp_auth: state.info.smtp_auth.clone(),
        api_auth: state.info.api_auth.clone(),
        api_tls: state.info.api_tls,
        smtp_tls: state.info.smtp_tls.clone(),
        tls_fingerprint: state.info.tls_fingerprint.clone(),
        faults_active: state.faults.is_active(),
    })
}

// ---------------------------------------------------------------------------
// Accounts
// ---------------------------------------------------------------------------

/// `GET /api/v1/accounts`.
///
/// # Errors
/// Propagates a storage failure as `500`.
#[utoipa::path(
    get, path = "/api/v1/accounts", tag = "accounts",
    responses((status = 200, description = "Every account", body = ListResponseAccountDto))
)]
pub async fn list_accounts(
    State(state): State<ApiState>,
) -> ApiResult<Json<ListResponse<AccountDto>>> {
    let accounts = state.accounts.list().await?;

    let mut items = Vec::with_capacity(accounts.len());
    for account in &accounts {
        let identities = state.accounts.identities(account.id).await?;
        let addresses = identities
            .iter()
            .map(|identity| identity.address.normalized())
            .collect();
        items.push(AccountDto::from_core(account, addresses));
    }

    let total = items.len() as u64;
    Ok(Json(ListResponse {
        items,
        next_cursor: None,
        total_estimate: total,
    }))
}

/// `GET /api/v1/accounts/{id}`.
///
/// # Errors
/// `400` for an unparseable id, `404` if no such account exists.
#[utoipa::path(
    get, path = "/api/v1/accounts/{id}", tag = "accounts",
    params(("id" = String, Path, description = "Account id (UUID)")),
    responses(
        (status = 200, description = "The account", body = AccountDto),
        (status = 400, description = "Malformed request", body = crate::error::ProblemDetails),
        (status = 404, description = "No such entity", body = crate::error::ProblemDetails),
    )
)]
pub async fn get_account(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> ApiResult<Json<AccountDto>> {
    let account_id = parse_id(&id, "account_id").map(sandesh_core::AccountId::from_uuid)?;
    let account = state
        .accounts
        .get(account_id)
        .await?
        .ok_or(ApiError::NotFound("Account"))?;

    let identities = state.accounts.identities(account.id).await?;
    let addresses = identities
        .iter()
        .map(|identity| identity.address.normalized())
        .collect();

    Ok(Json(AccountDto::from_core(&account, addresses)))
}

/// Hashes a new account password through the credential port.
///
/// It previously called `argon2` here directly, with the parameters copied by
/// hand from `sandesh-proto`. They matched — and would have stopped matching
/// the first time the canonical ones were retuned, silently, because nothing
/// compared them. Going through the port means there is exactly one Argon2
/// configuration in the process, and `tests/architecture.rs` now enforces that
/// the API cannot acquire a second (ADR-075).
fn hash_account_password(state: &ApiState, password: &str) -> ApiResult<String> {
    let verifier = state
        .credentials
        .as_ref()
        .ok_or_else(|| ApiError::Internal(String::from("no credential verifier is configured")))?;
    verifier.hash(password).map_err(ApiError::Internal)
}

/// `POST /api/v1/accounts` — creates an account, provisions default mailboxes, and binds addresses.
///
/// # Errors
/// `400` for invalid account data, `500` for storage failure.
#[utoipa::path(
    post, path = "/api/v1/accounts", tag = "accounts",
    request_body = CreateAccountRequest,
    responses(
        (status = 201, description = "The account was created", body = AccountDto),
        (status = 400, description = "Malformed request", body = crate::error::ProblemDetails),
    )
)]
pub async fn create_account(
    State(state): State<ApiState>,
    Json(payload): Json<CreateAccountRequest>,
) -> ApiResult<(StatusCode, Json<AccountDto>)> {
    let credential = if let Some(ref pass) = payload.password {
        // Hashing happens before the domain sees the credential: the core
        // declares no cryptography (ADR-070), so it arrives already hashed.
        let hash = hash_account_password(&state, pass)?;
        Some(Credential { hash, plain: None })
    } else {
        None
    };

    // Blank entries are dropped rather than rejected — a UI that sends an empty
    // row should not fail the request — but the service still refuses an account
    // left with no address at all, which could never receive mail.
    let addresses: Vec<EmailAddress> = payload
        .addresses
        .iter()
        .filter(|address| !address.trim().is_empty())
        .map(|address| EmailAddress::parse_lenient(address))
        .collect();
    let identities = addresses
        .iter()
        .map(EmailAddress::normalized)
        .collect::<Vec<_>>();

    let account = state
        .account_service
        .as_ref()
        .create(NewAccount {
            username: payload.username.clone(),
            addresses,
            credential,
            is_catch_all: payload.is_catch_all.unwrap_or(false),
        })
        .await
        .map_err(account_error)?;

    Ok((
        StatusCode::CREATED,
        Json(AccountDto::from_core(&account, identities)),
    ))
}

/// Renders an account-service failure with this endpoint's statuses.
///
/// A rejected username or address list is a `400`: the request is malformed
/// rather than merely unprocessable, and that is what this endpoint has always
/// answered for bad input.
fn account_error(error: sandesh_core::CoreError) -> ApiError {
    match error {
        sandesh_core::CoreError::InvalidArgument { ref reason, .. } => {
            ApiError::bad_request(reason.clone())
        }
        other => ApiError::from(other),
    }
}

/// `PATCH /api/v1/accounts/{id}` — updates account credentials or aliases.
///
/// # Errors
/// `400` for an unparseable id, `404` if no such account exists, `500` for storage failure.
#[utoipa::path(
    patch, path = "/api/v1/accounts/{id}", tag = "accounts",
    params(("id" = String, Path, description = "Account id (UUID)")),
    request_body = PatchAccountRequest,
    responses(
        (status = 200, description = "The account was updated", body = AccountDto),
        (status = 400, description = "Malformed request", body = crate::error::ProblemDetails),
        (status = 404, description = "No such entity", body = crate::error::ProblemDetails),
    )
)]
pub async fn patch_account(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(payload): Json<PatchAccountRequest>,
) -> ApiResult<Json<AccountDto>> {
    let account_id = parse_id(&id, "account_id").map(sandesh_core::AccountId::from_uuid)?;
    let mut account = state
        .accounts
        .get(account_id)
        .await?
        .ok_or(ApiError::NotFound("Account"))?;

    if let Some(ref pass) = payload.password {
        let hash = hash_account_password(&state, pass)?;
        let cred = Credential { hash, plain: None };
        state
            .accounts
            .update_credential(account_id, Some(&cred))
            .await?;
        account.credential = Some(cred);
    }

    if let Some(is_catch_all) = payload.is_catch_all {
        state
            .accounts
            .set_catch_all(account_id, is_catch_all)
            .await?;
        account.is_catch_all = is_catch_all;
    }

    for addr_str in &payload.add_addresses {
        if !addr_str.trim().is_empty() {
            let addr = EmailAddress::parse_lenient(addr_str);
            let _ = state.accounts.add_identity(account_id, &addr, false).await;
        }
    }

    for addr_str in &payload.remove_addresses {
        if !addr_str.trim().is_empty() {
            let addr = EmailAddress::parse_lenient(addr_str);
            let _ = state.accounts.remove_identity(account_id, &addr).await;
        }
    }

    let identities = state
        .accounts
        .identities(account_id)
        .await?
        .into_iter()
        .map(|i| i.address.normalized())
        .collect();

    Ok(Json(AccountDto::from_core(&account, identities)))
}

/// `DELETE /api/v1/accounts/{id}` — deletes an account and all mailboxes and entries it owns.
///
/// # Errors
/// `400` for an unparseable id, `404` if no such account exists, `500` for storage failure.
#[utoipa::path(
    delete, path = "/api/v1/accounts/{id}", tag = "accounts",
    params(("id" = String, Path, description = "Account id (UUID)")),
    responses(
        (status = 204, description = "The account was deleted"),
        (status = 400, description = "Malformed request", body = crate::error::ProblemDetails),
        (status = 404, description = "No such entity", body = crate::error::ProblemDetails),
    )
)]
pub async fn delete_account(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    let account_id = parse_id(&id, "account_id").map(sandesh_core::AccountId::from_uuid)?;
    if state.accounts.delete(account_id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound("Account"))
    }
}

// ---------------------------------------------------------------------------
// Mailboxes
// ---------------------------------------------------------------------------

/// Query parameters for the mailbox list.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct MailboxQuery {
    /// Restrict to one account.
    #[serde(default)]
    account_id: Option<String>,
}

/// `GET /api/v1/mailboxes`.
///
/// Without `account_id` this walks every account, which is the behaviour a
/// single-developer instance wants: one request shows all the mail there is.
///
/// # Errors
/// `400` for an unparseable `account_id`.
#[utoipa::path(
    get, path = "/api/v1/mailboxes", tag = "mailboxes",
    params(MailboxQuery),
    responses(
        (status = 200, description = "Mailboxes with counts", body = ListResponseMailboxDto),
        (status = 400, description = "Malformed request", body = crate::error::ProblemDetails),
    )
)]
pub async fn list_mailboxes(
    State(state): State<ApiState>,
    Query(query): Query<MailboxQuery>,
) -> ApiResult<Json<ListResponse<MailboxDto>>> {
    let account_ids = match &query.account_id {
        Some(raw) => vec![sandesh_core::AccountId::from_uuid(parse_id(
            raw,
            "account_id",
        )?)],
        None => state
            .accounts
            .list()
            .await?
            .into_iter()
            .map(|account| account.id)
            .collect(),
    };

    let mut items = Vec::new();
    for account_id in account_ids {
        for mailbox in state.mailboxes.list(account_id).await? {
            let counts = state.mailboxes.counts(mailbox.id).await?;
            items.push(MailboxDto::from_core(&mailbox, counts));
        }
    }

    let total = items.len() as u64;
    Ok(Json(ListResponse {
        items,
        next_cursor: None,
        total_estimate: total,
    }))
}

/// `GET /api/v1/mailboxes/{id}`.
///
/// # Errors
/// `400` for an unparseable id, `404` if no such mailbox exists.
#[utoipa::path(
    get, path = "/api/v1/mailboxes/{id}", tag = "mailboxes",
    params(("id" = String, Path, description = "Mailbox id (UUID)")),
    responses(
        (status = 200, description = "The mailbox", body = MailboxDto),
        (status = 400, description = "Malformed request", body = crate::error::ProblemDetails),
        (status = 404, description = "No such entity", body = crate::error::ProblemDetails),
    )
)]
pub async fn get_mailbox(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> ApiResult<Json<MailboxDto>> {
    let mailbox_id = MailboxId::from_uuid(parse_id(&id, "mailbox_id")?);
    let mailbox = state
        .mailboxes
        .get(mailbox_id)
        .await?
        .ok_or(ApiError::NotFound("Mailbox"))?;
    let counts = state.mailboxes.counts(mailbox.id).await?;

    Ok(Json(MailboxDto::from_core(&mailbox, counts)))
}

/// `GET /api/v1/mailboxes/{id}/entries` — the IMAP-faithful view.
///
/// # Errors
/// `400` for an unparseable id or page parameter, `404` if no such mailbox exists.
#[utoipa::path(
    get, path = "/api/v1/mailboxes/{id}/entries", tag = "mailboxes",
    params(("id" = String, Path, description = "Mailbox id (UUID)"), Pagination),
    responses(
        (status = 200, description = "Placements in this mailbox", body = ListResponseEntryDto),
        (status = 400, description = "Malformed request", body = crate::error::ProblemDetails),
        (status = 404, description = "No such entity", body = crate::error::ProblemDetails),
    )
)]
pub async fn list_mailbox_entries(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Query(page): Query<Pagination>,
) -> ApiResult<Json<ListResponse<EntryDto>>> {
    let mailbox_id = MailboxId::from_uuid(parse_id(&id, "mailbox_id")?);
    if state.mailboxes.get(mailbox_id).await?.is_none() {
        return Err(ApiError::NotFound("Mailbox"));
    }

    let limit = page.limit()?;
    let entries = state.messages.list_entries(mailbox_id).await?;
    let total = entries.len() as u64;

    // Entries are ordered by UID, which is monotonic and never reused, so a UID
    // is a stable cursor: a row inserted during paging appears after the cursor
    // rather than shifting rows the client has already seen, which is exactly
    // what offset pagination gets wrong (API_DESIGN §2).
    let after = page.cursor_uid()?;
    let mut page_items: Vec<&MessageEntry> = entries
        .iter()
        .filter(|entry| after.is_none_or(|uid| entry.uid.get() > uid))
        .collect();
    page_items.sort_by_key(|entry| entry.uid.get());

    let has_more = page_items.len() > limit as usize;
    page_items.truncate(limit as usize);

    let next_cursor = has_more
        .then(|| page_items.last().map(|entry| entry.uid.get().to_string()))
        .flatten();

    Ok(Json(ListResponse {
        items: page_items.into_iter().map(EntryDto::from_core).collect(),
        next_cursor,
        total_estimate: total,
    }))
}

/// Renders a mailbox-service failure, keeping INBOX refusals as `400`.
///
/// The service reports "INBOX cannot be created/renamed/deleted" as
/// [`sandesh_core::CoreError::InvalidArgument`], which the blanket mapping
/// turns into `422`. These endpoints have always answered `400` for it and that
/// is the documented contract, so the status is pinned here rather than letting
/// a refactor of where the *rule* lives silently change what clients see. Every
/// other error keeps the standard mapping.
fn mailbox_error(error: sandesh_core::CoreError) -> ApiError {
    match error {
        sandesh_core::CoreError::InvalidArgument { ref reason, .. } => {
            ApiError::bad_request(reason.clone())
        }
        other => ApiError::from(other),
    }
}

/// `POST /api/v1/mailboxes` — creates a new mailbox (and any missing parents).
///
/// # Errors
/// `400` for invalid mailbox name or unparseable account id, `500` for storage failure.
#[utoipa::path(
    post, path = "/api/v1/mailboxes", tag = "mailboxes",
    request_body = CreateMailboxRequest,
    responses(
        (status = 201, description = "The mailbox was created", body = MailboxDto),
        (status = 400, description = "Malformed request", body = crate::error::ProblemDetails),
        (status = 404, description = "Account not found", body = crate::error::ProblemDetails),
    )
)]
pub async fn create_mailbox(
    State(state): State<ApiState>,
    Json(payload): Json<CreateMailboxRequest>,
) -> ApiResult<(StatusCode, Json<MailboxDto>)> {
    let account_id = if let Some(ref acc_str) = payload.account_id {
        AccountId::from_uuid(parse_id(acc_str, "account_id")?)
    } else {
        let accounts = state.accounts.list().await?;
        accounts
            .first()
            .map(|a| a.id)
            .ok_or(ApiError::NotFound("Account"))?
    };

    let name = MailboxName::new(&payload.name)
        .map_err(|e| ApiError::bad_request(format!("invalid mailbox name: {e}")))?;

    // The domain service owns what CREATE means: INBOX is refused, a taken
    // name is a conflict rather than a storage error, and a new mailbox starts
    // unsubscribed. IMAP already behaved this way; this handler did not
    // (ADR-068).
    let created = state
        .mailbox_service
        .as_ref()
        .create(account_id, &name, MailboxAttributes::none())
        .await
        .map_err(mailbox_error)?;

    let counts = state.mailboxes.counts(created.id).await.unwrap_or_default();
    Ok((
        StatusCode::CREATED,
        Json(MailboxDto::from_core(&created, counts)),
    ))
}

/// `PATCH /api/v1/mailboxes/{id}` — renames or updates subscription for a mailbox.
///
/// # Errors
/// `400` for invalid name or renaming INBOX, `404` if not found, `500` for storage failure.
#[utoipa::path(
    patch, path = "/api/v1/mailboxes/{id}", tag = "mailboxes",
    params(("id" = String, Path, description = "Mailbox id (UUID)")),
    request_body = PatchMailboxRequest,
    responses(
        (status = 200, description = "The mailbox was updated", body = MailboxDto),
        (status = 400, description = "Malformed request", body = crate::error::ProblemDetails),
        (status = 404, description = "No such entity", body = crate::error::ProblemDetails),
    )
)]
pub async fn patch_mailbox(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(payload): Json<PatchMailboxRequest>,
) -> ApiResult<Json<MailboxDto>> {
    let mailbox_id = MailboxId::from_uuid(parse_id(&id, "mailbox_id")?);
    let mut mailbox = state
        .mailboxes
        .get(mailbox_id)
        .await?
        .ok_or(ApiError::NotFound("Mailbox"))?;

    let mailboxes = state.mailbox_service.as_ref();

    if let Some(ref new_name_str) = payload.name {
        let new_name = MailboxName::new(new_name_str)
            .map_err(|e| ApiError::bad_request(format!("invalid mailbox name: {e}")))?;

        // The service refuses INBOX as *either* side of the rename. This
        // handler previously checked only the current name, so a mailbox could
        // be renamed onto INBOX — which breaks the one-INBOX-per-account
        // invariant delivery depends on (ADR-068).
        mailboxes
            .rename(mailbox.account_id, &mailbox.name, &new_name)
            .await
            .map_err(mailbox_error)?;

        mailbox = state
            .mailboxes
            .get(mailbox_id)
            .await?
            .ok_or(ApiError::NotFound("Mailbox"))?;
    }

    if let Some(subscribed) = payload.subscribed {
        mailboxes
            .set_subscribed(mailbox.account_id, &mailbox.name, subscribed)
            .await
            .map_err(mailbox_error)?;
        mailbox.subscribed = subscribed;
    }

    let counts = state.mailboxes.counts(mailbox_id).await.unwrap_or_default();
    Ok(Json(MailboxDto::from_core(&mailbox, counts)))
}

/// `DELETE /api/v1/mailboxes/{id}` — deletes a mailbox and its placements.
///
/// # Errors
/// `400` for deleting INBOX, `404` if not found, `500` for storage failure.
#[utoipa::path(
    delete, path = "/api/v1/mailboxes/{id}", tag = "mailboxes",
    params(("id" = String, Path, description = "Mailbox id (UUID)")),
    responses(
        (status = 204, description = "The mailbox was deleted"),
        (status = 400, description = "Malformed request or cannot delete INBOX", body = crate::error::ProblemDetails),
        (status = 404, description = "No such entity", body = crate::error::ProblemDetails),
    )
)]
pub async fn delete_mailbox(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    let mailbox_id = MailboxId::from_uuid(parse_id(&id, "mailbox_id")?);
    let mailbox = state
        .mailboxes
        .get(mailbox_id)
        .await?
        .ok_or(ApiError::NotFound("Mailbox"))?;

    // The INBOX refusal is the service's, via `MailboxName::is_inbox` rather
    // than a second case-insensitive comparison written out here.
    state
        .mailbox_service
        .as_ref()
        .delete(mailbox.account_id, &mailbox.name)
        .await
        .map_err(mailbox_error)?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Entries
// ---------------------------------------------------------------------------

/// `GET /api/v1/entries/{id}`.
///
/// # Errors
/// `400` for an unparseable id, `404` if no such placement exists.
#[utoipa::path(
    get, path = "/api/v1/entries/{id}", tag = "entries",
    params(("id" = String, Path, description = "Entry id (UUID)")),
    responses(
        (status = 200, description = "The placement", body = EntryDto),
        (status = 400, description = "Malformed request", body = crate::error::ProblemDetails),
        (status = 404, description = "No such entity", body = crate::error::ProblemDetails),
    )
)]
pub async fn get_entry(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> ApiResult<Json<EntryDto>> {
    let entry_id = EntryId::from_uuid(parse_id(&id, "entry_id")?);
    let entry = state
        .messages
        .get_entry(entry_id)
        .await?
        .ok_or(ApiError::NotFound("Entry"))?;

    Ok(Json(EntryDto::from_core(&entry)))
}

/// Body of a flag update (`PATCH /api/v1/entries/{id}/flags`).
#[derive(Debug, Deserialize, ToSchema)]
pub struct FlagPatch {
    /// Flag names to add.
    #[serde(default)]
    add: Vec<String>,
    /// Flag names to remove.
    #[serde(default)]
    remove: Vec<String>,
}

/// `PATCH /api/v1/entries/{id}/flags` — maps to IMAP `STORE`.
///
/// # Errors
/// `400` for an unparseable id, `422` for an unusable flag name, `404` if no such placement exists.
#[utoipa::path(
    patch, path = "/api/v1/entries/{id}/flags", tag = "entries",
    params(("id" = String, Path, description = "Entry id (UUID)")),
    request_body = FlagPatch,
    responses(
        (status = 200, description = "The updated placement", body = EntryDto),
        (status = 400, description = "Malformed request", body = crate::error::ProblemDetails),
        (status = 404, description = "No such entity", body = crate::error::ProblemDetails),
        (status = 422, description = "Unusable flag name", body = crate::error::ProblemDetails),
    )
)]
pub async fn patch_entry_flags(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(patch): Json<FlagPatch>,
) -> ApiResult<Json<EntryDto>> {
    let entry_id = EntryId::from_uuid(parse_id(&id, "entry_id")?);

    let (add_system, add_keywords) = parse_flags(&patch.add)?;
    let (remove_system, remove_keywords) = parse_flags(&patch.remove)?;

    // Reject an unknown entry before mutating anything, so a bad id cannot
    // half-apply an update.
    if state.messages.get_entry(entry_id).await?.is_none() {
        return Err(ApiError::NotFound("Entry"));
    }

    // Additions first, then removals: a name in both lists ends up removed,
    // which is the least surprising reading of a contradictory request.
    if !add_system.is_empty() || !add_keywords.is_empty() {
        state
            .messages
            .update_flags(
                entry_id,
                FlagUpdate::Add {
                    system: add_system,
                    keywords: add_keywords,
                },
            )
            .await?;
    }
    if !remove_system.is_empty() || !remove_keywords.is_empty() {
        state
            .messages
            .update_flags(
                entry_id,
                FlagUpdate::Remove {
                    system: remove_system,
                    keywords: remove_keywords,
                },
            )
            .await?;
    }

    let entry = state
        .messages
        .get_entry(entry_id)
        .await?
        .ok_or(ApiError::NotFound("Entry"))?;
    Ok(Json(EntryDto::from_core(&entry)))
}

/// Query parameters for `DELETE /api/v1/entries/{id}`.
#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct DeleteEntryQuery {
    /// If true, permanently purges the entry rather than moving it to Trash.
    #[serde(default)]
    pub permanent: Option<bool>,
}

/// `DELETE /api/v1/entries/{id}` — moves entry to Trash, or permanently purges if in Trash or ?permanent=true.
///
/// # Errors
/// Returns `400` if the id is not a UUID and `404` if no such entry exists.
#[utoipa::path(
    delete, path = "/api/v1/entries/{id}", tag = "entries",
    params(
        ("id" = String, Path, description = "Entry id (UUID)"),
        DeleteEntryQuery,
    ),
    responses(
        (status = 204, description = "The placement was removed or moved to Trash"),
        (status = 400, description = "Malformed request", body = crate::error::ProblemDetails),
        (status = 404, description = "No such entity", body = crate::error::ProblemDetails),
    )
)]
pub async fn delete_entry(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Query(query): Query<DeleteEntryQuery>,
) -> ApiResult<StatusCode> {
    let entry_id = EntryId::from_uuid(parse_id(&id, "entry_id")?);
    // The service resolves the placement, so a missing one is its `NotFound`
    // rather than a lookup repeated here.
    let messages = state.message_service.as_ref();

    // `?permanent=true` skips the Trash hop; otherwise the service decides,
    // resolving Trash from the placement's own account (ADR-069).
    if query.permanent.unwrap_or(false) {
        messages.remove(entry_id).await?;
    } else {
        messages.discard(entry_id).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/v1/entries/{id}/move` — moves placement to another mailbox.
///
/// # Errors
/// Returns `400` if either id is not a UUID and `404` if the entry or the
/// target mailbox does not exist.
#[utoipa::path(
    post, path = "/api/v1/entries/{id}/move", tag = "entries",
    params(("id" = String, Path, description = "Entry id (UUID)")),
    request_body = MoveEntryRequest,
    responses(
        (status = 200, description = "The placement was moved", body = EntryDto),
        (status = 400, description = "Malformed request", body = crate::error::ProblemDetails),
        (status = 404, description = "No such entity", body = crate::error::ProblemDetails),
    )
)]
pub async fn move_entry(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(payload): Json<MoveEntryRequest>,
) -> ApiResult<Json<EntryDto>> {
    let entry_id = EntryId::from_uuid(parse_id(&id, "entry_id")?);
    let entry = state
        .messages
        .get_entry(entry_id)
        .await?
        .ok_or(ApiError::NotFound("Entry"))?;

    let target_mailbox_id = resolve_entry_destination(
        &state,
        &entry,
        payload.target_mailbox_id.as_deref(),
        payload.mailbox_name.as_deref(),
    )
    .await?;

    let moved = state
        .messages
        .move_entry(entry_id, target_mailbox_id)
        .await?;
    Ok(Json(EntryDto::from_core(&moved)))
}

/// `POST /api/v1/entries/{id}/copy` — copies placement to another mailbox.
///
/// # Errors
/// Returns `400` if either id is not a UUID and `404` if the entry or the
/// target mailbox does not exist.
#[utoipa::path(
    post, path = "/api/v1/entries/{id}/copy", tag = "entries",
    params(("id" = String, Path, description = "Entry id (UUID)")),
    request_body = CopyEntryRequest,
    responses(
        (status = 200, description = "The placement was copied", body = EntryDto),
        (status = 400, description = "Malformed request", body = crate::error::ProblemDetails),
        (status = 404, description = "No such entity", body = crate::error::ProblemDetails),
    )
)]
pub async fn copy_entry(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(payload): Json<CopyEntryRequest>,
) -> ApiResult<Json<EntryDto>> {
    let entry_id = EntryId::from_uuid(parse_id(&id, "entry_id")?);
    let entry = state
        .messages
        .get_entry(entry_id)
        .await?
        .ok_or(ApiError::NotFound("Entry"))?;

    let target_mailbox_id = resolve_entry_destination(
        &state,
        &entry,
        payload.target_mailbox_id.as_deref(),
        payload.mailbox_name.as_deref(),
    )
    .await?;

    let copied = state
        .messages
        .copy_entry(entry_id, target_mailbox_id)
        .await?;
    Ok(Json(EntryDto::from_core(&copied)))
}

/// `POST /api/v1/entries/bulk` — applies actions across multiple placements.
///
/// Supports marking read, marking unread, moving, or deleting a list of entries.
///
/// # Errors
/// Returns `400` if parameters are invalid.
#[utoipa::path(
    post, path = "/api/v1/entries/bulk", tag = "entries",
    request_body = BulkEntriesRequest,
    responses(
        (status = 200, description = "The bulk operation completed", body = BulkEntriesResponse),
        (status = 400, description = "Malformed request", body = crate::error::ProblemDetails),
    )
)]
pub async fn bulk_entries(
    State(state): State<ApiState>,
    Json(payload): Json<BulkEntriesRequest>,
) -> ApiResult<Json<BulkEntriesResponse>> {
    let mut affected = 0;

    for id_str in &payload.entry_ids {
        let Ok(uuid) = parse_id(id_str, "entry_id") else {
            continue;
        };
        let entry_id = EntryId::from_uuid(uuid);

        let Some(entry) = state.messages.get_entry(entry_id).await? else {
            continue;
        };

        // Each arm reports whether it changed the entry, so the counter is
        // incremented in exactly one place. The two arms that need to resolve
        // a destination mailbox live in their own functions, because that
        // resolution is the complicated part and it is not what this loop is
        // about.
        let changed = match &payload.action {
            BulkAction::MarkRead => set_seen(&state, entry_id, true).await,
            BulkAction::MarkUnread => set_seen(&state, entry_id, false).await,
            BulkAction::Delete => bulk_delete_entry(&state, entry_id).await?,
            BulkAction::Move {
                target_mailbox_id,
                mailbox_name,
            } => {
                bulk_move_entry(
                    &state,
                    entry_id,
                    &entry,
                    target_mailbox_id.as_deref(),
                    mailbox_name.as_deref(),
                )
                .await
            }
        };

        if changed {
            affected += 1;
        }
    }

    Ok(Json(BulkEntriesResponse { affected }))
}

/// Adds or removes `\Seen` on one entry, reporting whether it changed.
async fn set_seen(state: &ApiState, entry_id: EntryId, seen: bool) -> bool {
    let update = if seen {
        FlagUpdate::Add {
            system: SystemFlags::SEEN,
            keywords: BTreeSet::new(),
        }
    } else {
        FlagUpdate::Remove {
            system: SystemFlags::SEEN,
            keywords: BTreeSet::new(),
        }
    };
    state.messages.update_flags(entry_id, update).await.is_ok()
}

/// Deletes one entry the way a mail client expects: to `Trash`, or for real.
///
/// An entry already in `Trash` is deleted outright; anything else is moved
/// there, which is what makes the action recoverable. If no `Trash` mailbox
/// exists the entry is deleted, because refusing would leave the user unable
/// to delete anything at all.
///
/// # Errors
///
/// Propagates a mailbox lookup failure. A failed *delete* or *move* is not an
/// error: it reports `false` so the caller can carry on with the rest of the
/// batch.
async fn bulk_delete_entry(state: &ApiState, entry_id: EntryId) -> ApiResult<bool> {
    // One implementation of the Trash policy, shared with `DELETE /entries/{id}`.
    // The copy this replaced fell back to a freshly generated `AccountId` when
    // the source mailbox could not be read — an id that owns no Trash, so the
    // lookup could never succeed (ADR-069).
    Ok(state
        .message_service
        .as_ref()
        .discard(entry_id)
        .await
        .is_ok())
}

/// Moves one entry to a mailbox named either by id or by name.
///
/// Both destination forms go through [`resolve_entry_destination`], so a bulk
/// move is bound by the same account check as the single-entry endpoint — the
/// id form previously bypassed it here too. An unresolvable or refused
/// destination reports `false` rather than failing the whole batch, which is
/// what makes this function's result the per-entry outcome the caller counts.
async fn bulk_move_entry(
    state: &ApiState,
    entry_id: EntryId,
    entry: &MessageEntry,
    target_mailbox_id: Option<&str>,
    mailbox_name: Option<&str>,
) -> bool {
    let Ok(target) = resolve_entry_destination(state, entry, target_mailbox_id, mailbox_name).await
    else {
        return false;
    };

    state.messages.move_entry(entry_id, target).await.is_ok()
}

/// Resolves a copy/move destination given either of the two forms the request
/// bodies accept, and hands back the target id for the caller to act on.
///
/// The rules — same account, able to hold mail, not the entry's current mailbox
/// — now live in `MessageService` (ADR-069), so IMAP and HTTP enforce one
/// implementation. This function is the translation layer: it turns the two
/// request shapes into a `MailboxId` and renders the domain's refusals as the
/// statuses this endpoint has always used.
///
/// # Errors
/// `400` when an id is not a UUID or neither form is supplied; `404` when the
/// destination does not exist or belongs to another account (deliberately
/// indistinguishable, so the endpoint is not an id oracle); `409` when it
/// cannot hold mail or is already the entry's mailbox.
async fn resolve_entry_destination(
    state: &ApiState,
    entry: &MessageEntry,
    target_mailbox_id: Option<&str>,
    mailbox_name: Option<&str>,
) -> ApiResult<MailboxId> {
    let messages = state.message_service.as_ref();

    if let Some(id) = target_mailbox_id {
        let candidate = MailboxId::from_uuid(parse_id(id, "target_mailbox_id")?);
        // Resolving by id runs every check without performing the copy, so an
        // unusable destination is reported before anything is written.
        let destination = messages
            .resolve_destination_by_id(entry, candidate)
            .await
            .map_err(entry_destination_error)?;
        Ok(destination.id)
    } else if let Some(name) = mailbox_name {
        let parsed = MailboxName::new(name)
            .map_err(|e| ApiError::bad_request(format!("invalid mailbox name: {e}")))?;
        let destination = messages
            .resolve_destination_named(entry, &parsed)
            .await
            .map_err(entry_destination_error)?;
        Ok(destination.id)
    } else {
        Err(ApiError::bad_request(
            "either target_mailbox_id or mailbox_name must be provided",
        ))
    }
}

/// Renders a destination refusal with the statuses these endpoints document.
///
/// A missing mailbox is `404 Target Mailbox` rather than the generic entity
/// name, so the message says which mailbox was not found.
fn entry_destination_error(error: sandesh_core::CoreError) -> ApiError {
    match error {
        sandesh_core::CoreError::NotFound { .. } => ApiError::NotFound("Target Mailbox"),
        other => ApiError::from(other),
    }
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

/// Pagination and filtering for the message list.
#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct Pagination {
    /// Page size; defaults to [`DEFAULT_LIMIT`], capped at [`MAX_LIMIT`].
    #[serde(default)]
    limit: Option<u32>,
    /// Opaque cursor from the previous page.
    #[serde(default)]
    cursor: Option<String>,
    /// Restrict to one mailbox.
    #[serde(default)]
    mailbox_id: Option<String>,
    /// Restrict to one account.
    #[serde(default)]
    account_id: Option<String>,
}

impl Pagination {
    /// The validated page size.
    fn limit(&self) -> ApiResult<u32> {
        match self.limit {
            None => Ok(DEFAULT_LIMIT),
            Some(0) => Err(ApiError::bad_request("limit must be at least 1")),
            Some(value) if value > MAX_LIMIT => Err(ApiError::bad_request(format!(
                "limit must not exceed {MAX_LIMIT}"
            ))),
            Some(value) => Ok(value),
        }
    }

    /// The cursor parsed as a UID.
    fn cursor_uid(&self) -> ApiResult<Option<u32>> {
        self.cursor
            .as_deref()
            .map(|raw| {
                raw.parse::<u32>()
                    .map_err(|_| ApiError::bad_request("cursor is not valid"))
            })
            .transpose()
    }
}

/// `GET /api/v1/messages`.
///
/// Returns summaries, not full messages: a page of 50 must not carry 50 message
/// bodies. Each summary still carries **all** of its entries, so a message in
/// two mailboxes is visible as one message with two placements in a single
/// request (`API_DESIGN.md` §4).
///
/// # Errors
/// `400` for an unparseable page parameter or identifier.
#[utoipa::path(
    get, path = "/api/v1/messages", tag = "messages",
    params(Pagination),
    responses(
        (status = 200, description = "A page of message summaries", body = ListResponseMessageSummary),
        (status = 400, description = "Malformed request", body = crate::error::ProblemDetails),
    )
)]
pub async fn list_messages(
    State(state): State<ApiState>,
    Query(page): Query<Pagination>,
) -> ApiResult<Json<ListResponse<MessageSummary>>> {
    let limit = page.limit()?;
    let after = page.cursor_uid()?;

    // Which mailboxes are in scope. Filtering by mailbox is the common case and
    // reads only that mailbox's entries rather than everything.
    let mailboxes = scope_mailboxes(&state, &page).await?;

    // Collect entries per message so a message placed in several in-scope
    // mailboxes appears once, carrying every placement.
    let mut names = BTreeMap::new();
    let mut per_message: BTreeMap<MessageId, Vec<MessageEntry>> = BTreeMap::new();
    for mailbox_id in mailboxes {
        let Some(mailbox) = state.mailboxes.get(mailbox_id).await? else {
            continue;
        };
        names.insert(mailbox_id, mailbox.name.as_str().to_owned());

        for entry in state.messages.list_entries(mailbox_id).await? {
            // The cursor walks *backwards*, because this list is newest-first
            // and UIDs ascend with age (§2 of API_DESIGN). Reading it forwards
            // — `uid > after` — returns the rows the client has already seen
            // and then reports the list exhausted, which truncated every
            // mailbox at two pages regardless of its size.
            if after.is_some_and(|uid| entry.uid.get() >= uid) {
                continue;
            }
            per_message.entry(entry.message_id).or_default().push(entry);
        }
    }

    let total = per_message.len() as u64;

    // Newest first: a developer watching mail arrive wants the last thing sent.
    let mut ordered: Vec<_> = per_message.into_iter().collect();
    ordered.sort_by(|a, b| {
        let newest = |entries: &Vec<MessageEntry>| {
            entries
                .iter()
                .map(|entry| entry.added_at)
                .max()
                .unwrap_or_else(|| sandesh_core::Timestamp::from_micros(0))
        };
        newest(&b.1).cmp(&newest(&a.1))
    });

    let has_more = ordered.len() > limit as usize;
    ordered.truncate(limit as usize);

    let mut items = Vec::with_capacity(ordered.len());
    for (message_id, entries) in ordered {
        let Some(message) = state.messages.get(message_id).await? else {
            // The message was collected between listing entries and reading it.
            // Skipping is correct: it no longer exists to report.
            continue;
        };
        items.push(MessageSummary::from_core(
            &message,
            entry_refs(&entries, &names),
        ));
    }

    // The cursor is the *lowest* UID on this page, since the next page holds
    // older messages. Taking the first entry of the last item is only correct
    // when that entry is the smallest one the item carries — a message placed
    // in several mailboxes carries several UIDs — so the minimum is taken
    // explicitly rather than relying on placement order.
    let next_cursor = has_more
        .then(|| {
            items
                .last()
                .and_then(|item| item.entries.iter().map(|entry| entry.uid).min())
                .map(|uid| uid.to_string())
        })
        .flatten();

    Ok(Json(ListResponse {
        items,
        next_cursor,
        total_estimate: total,
    }))
}

/// `POST /api/v1/messages` — direct injection of an RFC 822 / MIME message.
///
/// Accepts the raw message octets directly in the request body, parsing and placing
/// it into recipient inboxes (or the catch-all account) without going through SMTP.
///
/// # Errors
/// Returns `400` if the body is empty.
#[utoipa::path(
    post, path = "/api/v1/messages", tag = "messages",
    request_body(content = String, description = "Raw RFC 822 / MIME message octets", content_type = "message/rfc822"),
    responses(
        (status = 201, description = "The message was stored and placed", body = MessageSummary),
        (status = 400, description = "Malformed request", body = crate::error::ProblemDetails),
    )
)]
pub async fn inject_message(
    State(state): State<ApiState>,
    body: Bytes,
) -> ApiResult<(StatusCode, Json<MessageSummary>)> {
    if body.is_empty() {
        return Err(ApiError::bad_request("message body cannot be empty"));
    }

    let inserted = place_raw_message(&state, body.to_vec(), None, None).await?;
    let message = state
        .messages
        .get(inserted.message_id)
        .await?
        .ok_or(ApiError::NotFound("Message"))?;

    let names = mailbox_names(&state, &inserted.entries).await?;
    let summary = MessageSummary::from_core(&message, entry_refs(&inserted.entries, &names));

    Ok((StatusCode::CREATED, Json(summary)))
}

/// Stores raw message bytes and places them, as `POST /messages` does.
///
/// Factored out of [`inject_message`] because replay and import both need
/// exactly this and nothing else. Keeping one implementation is what makes
/// "a replayed message is placed the same way an injected one is" structural
/// rather than three routing rules that have to be kept in agreement — the
/// same argument ADR-088 made for `DATA` and `BDAT` sharing a tail.
///
/// `replay_of` names the source message when this is a replay, and is recorded
/// on the delivery. The `X-Sandesh-Replay-Of` header is written by the caller,
/// into the bytes, before they reach here.
async fn place_raw_message(
    state: &ApiState,
    bytes: Vec<u8>,
    replay_of: Option<MessageId>,
    target_mailbox_id: Option<sandesh_core::MailboxId>,
) -> ApiResult<sandesh_core::InsertedMessage> {
    let started = Timestamp::now();
    let now = started;
    let raw = RawMessage::new(bytes);
    let parsed = sandesh_core::parse(raw.bytes());

    let sender_addr = parsed.header("From").map_or_else(
        || EmailAddress::parse_lenient("unknown@localhost"),
        |h| EmailAddress::parse_lenient(&h.decoded_value),
    );

    let mut all_recipients = Vec::new();
    for h in parsed.headers_named("To") {
        all_recipients.push(EmailAddress::parse_lenient(&h.decoded_value));
    }
    for h in parsed.headers_named("Cc") {
        all_recipients.push(EmailAddress::parse_lenient(&h.decoded_value));
    }
    for h in parsed.headers_named("Bcc") {
        all_recipients.push(EmailAddress::parse_lenient(&h.decoded_value));
    }

    let mut placements = Vec::new();

    if let Some(target_id) = target_mailbox_id {
        let mbx = state.mailboxes.get(target_id).await?.ok_or_else(|| {
            ApiError::bad_request(format!("destination mailbox '{target_id}' not found"))
        })?;

        let flags = if mbx.name.as_str().eq_ignore_ascii_case("drafts") {
            MessageFlags::from_system(SystemFlags::SEEN.union(SystemFlags::DRAFT))
        } else {
            MessageFlags::empty()
        };

        placements.push(NewPlacement {
            mailbox_id: target_id,
            flags,
            internal_date: now,
        });
    } else {
        let inbox_name = MailboxName::new(INBOX).map_err(|e| ApiError::Internal(e.to_string()))?;

        for addr in &all_recipients {
            if let Ok(Some(account_id)) = state.accounts.resolve_address(addr).await
                && let Ok(Some(inbox)) = state.mailboxes.get_by_name(account_id, &inbox_name).await
                && !placements
                    .iter()
                    .any(|p: &NewPlacement| p.mailbox_id == inbox.id)
            {
                placements.push(NewPlacement {
                    mailbox_id: inbox.id,
                    flags: MessageFlags::empty(),
                    internal_date: now,
                });
            }
        }

        if placements.is_empty() {
            let fallback_acc_id = match state.accounts.catch_all().await {
                Ok(Some(catch_all)) => Some(catch_all.id),
                _ => match state.accounts.list().await {
                    Ok(mut list) if !list.is_empty() => Some(list.remove(0).id),
                    _ => None,
                },
            };

            if let Some(acc_id) = fallback_acc_id
                && let Ok(Some(inbox)) = state.mailboxes.get_by_name(acc_id, &inbox_name).await
            {
                placements.push(NewPlacement {
                    mailbox_id: inbox.id,
                    flags: MessageFlags::empty(),
                    internal_date: now,
                });
            }
        }
    }

    let envelope = Envelope {
        mail_from: Some(sender_addr),
        rcpt_to: all_recipients,
        helo_domain: Some(String::from("localhost")),
        peer: Some(String::from("127.0.0.1:api")),
        tls: None,
        auth_identity: None,
    };

    let new_message = NewMessage {
        raw,
        envelope,
        source: DeliverySource::Api,
        parsed,
        received_at: now,
        placements,
    };

    let envelope_for_record = new_message.envelope.clone();
    let inserted = state.messages.insert(&new_message).await?;

    record_api_delivery(state, &inserted, envelope_for_record, started, replay_of).await?;

    Ok(inserted)
}

/// Records the delivery for an API placement.
///
/// Written for every API placement, not only replays: without a record there is
/// nowhere to hang `replay_of`, and a replayed message whose provenance is
/// unqueryable meets only half the decision this increment made.
/// `DeliveryService` owns the SMTP path; this is the API path, which has no
/// envelope negotiation to record.
async fn record_api_delivery(
    state: &ApiState,
    inserted: &sandesh_core::InsertedMessage,
    envelope: Envelope,
    started: Timestamp,
    replay_of: Option<MessageId>,
) -> ApiResult<()> {
    // One outcome per envelope recipient, paired with the placement it
    // produced. Recipients that resolved to no account produced no placement
    // and are recorded as catch-all, which is what the placement fallback
    // actually did.
    let mut entry_ids = inserted.entries.iter().map(|entry| entry.id);
    let outcomes: Vec<RecipientOutcome> = envelope
        .rcpt_to
        .iter()
        .map(|rcpt| {
            let entry_id = entry_ids.next();
            RecipientOutcome {
                rcpt_to: rcpt.clone(),
                resolved_account: None,
                entry_id,
                outcome: if entry_id.is_some() {
                    DeliveryOutcome::Delivered
                } else {
                    DeliveryOutcome::CatchAll
                },
                reason: None,
            }
        })
        .collect();

    let delivery = Delivery {
        id: DeliveryId::new(),
        message_id: Some(inserted.message_id),
        source: DeliverySource::Api,
        envelope,
        outcomes,
        received_at: started,
        duration_us: Timestamp::now().as_micros() - started.as_micros(),
        connection_id: None,
        rules_applied: Vec::new(),
        replay_of,
    };

    state.deliveries.record(&delivery).await?;
    Ok(())
}

/// Query parameters for `GET /api/v1/search`.
#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SearchParams {
    /// Filter expression (e.g. `from:alice is:unread has:attachment report`).
    #[serde(default)]
    pub q: Option<String>,
    /// Restrict to one mailbox.
    #[serde(default)]
    pub mailbox_id: Option<String>,
    /// Restrict to one account.
    #[serde(default)]
    pub account_id: Option<String>,
    /// Page size.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Cursor from previous page.
    #[serde(default)]
    pub cursor: Option<String>,
}

/// `GET /api/v1/search`.
///
/// Searches messages using developer filter syntax (`from:`, `to:`, `subject:`,
/// `is:unread`, `has:attachment`, `larger:`, `smaller:` and free text terms).
///
/// # Errors
/// `400` for an unparseable page parameter or identifier.
#[utoipa::path(
    get, path = "/api/v1/search", tag = "search",
    params(SearchParams),
    responses(
        (status = 200, description = "A page of matching message summaries", body = ListResponseMessageSummary),
        (status = 400, description = "Malformed request", body = crate::error::ProblemDetails),
    )
)]
pub async fn search_messages(
    State(state): State<ApiState>,
    Query(params): Query<SearchParams>,
) -> ApiResult<Json<ListResponse<MessageSummary>>> {
    let pagination = Pagination {
        limit: params.limit,
        cursor: params.cursor,
        mailbox_id: params.mailbox_id,
        account_id: params.account_id,
    };
    let limit = pagination.limit()?;
    let after = pagination.cursor_uid()?;

    let query = parse_filter_query(params.q.as_deref().unwrap_or(""));
    let mailboxes = scope_mailboxes(&state, &pagination).await?;

    // Ask the index which messages are worth loading. This changes only how
    // many messages are fetched, never which ones match: `candidates` is a
    // superset, `admits` lets everything through when the index cannot help,
    // and `query.matches` below still decides every result (STORAGE_DESIGN §7).
    let candidates = state.search_index.candidates(&query).await?;

    let mut names = BTreeMap::new();
    let mut per_message: BTreeMap<MessageId, Vec<MessageEntry>> = BTreeMap::new();

    for mailbox_id in mailboxes {
        let Some(mailbox) = state.mailboxes.get(mailbox_id).await? else {
            continue;
        };
        names.insert(mailbox_id, mailbox.name.as_str().to_owned());

        for entry in state.messages.list_entries(mailbox_id).await? {
            if after.is_some_and(|uid| entry.uid.get() <= uid) {
                continue;
            }

            // A message the index has excluded provably cannot match, so it is
            // not loaded at all — the saving the index exists for.
            if !candidates.admits(entry.message_id) {
                continue;
            }

            let msg = if query.needs_message() {
                state.messages.get(entry.message_id).await?
            } else {
                None
            };

            if query.matches(msg.as_ref(), &entry) {
                per_message.entry(entry.message_id).or_default().push(entry);
            }
        }
    }

    let total = per_message.len() as u64;

    let mut ordered: Vec<_> = per_message.into_iter().collect();
    ordered.sort_by(|a, b| {
        let newest = |entries: &Vec<MessageEntry>| {
            entries
                .iter()
                .map(|entry| entry.added_at)
                .max()
                .unwrap_or_else(|| sandesh_core::Timestamp::from_micros(0))
        };
        newest(&b.1).cmp(&newest(&a.1))
    });

    let has_more = ordered.len() > limit as usize;
    ordered.truncate(limit as usize);

    let mut items = Vec::with_capacity(ordered.len());
    for (message_id, entries) in ordered {
        let Some(message) = state.messages.get(message_id).await? else {
            continue;
        };
        items.push(MessageSummary::from_core(
            &message,
            entry_refs(&entries, &names),
        ));
    }

    let next_cursor = has_more
        .then(|| {
            items
                .last()
                .and_then(|item| item.entries.first())
                .map(|entry| entry.uid.to_string())
        })
        .flatten();

    Ok(Json(ListResponse {
        items,
        next_cursor,
        total_estimate: total,
    }))
}

/// Query parameters for `GET /api/v1/search/explain`.
#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ExplainParams {
    /// Filter expression to explain.
    #[serde(default)]
    pub q: Option<String>,
}

/// `GET /api/v1/search/explain`.
///
/// Explains how a search filter string is compiled into the domain query plan.
#[utoipa::path(
    get, path = "/api/v1/search/explain", tag = "search",
    params(ExplainParams),
    responses(
        (status = 200, description = "The parsed query plan", body = SearchExplainResponse),
    )
)]
// Infallible in practice: an absent or unparseable `q` yields `SearchQuery::All`
// rather than an error. The `Result` is kept for consistency with the other
// handlers, so adding a fallible step later will not change this signature.
//
// `# Errors` is deliberately not a doc comment: `utoipa` appends doc text to the
// published OpenAPI description, and an internal note about error paths is not
// part of the API contract clients read.
#[allow(clippy::missing_errors_doc, reason = "see the comment above")]
pub async fn explain_search(
    Query(params): Query<ExplainParams>,
) -> ApiResult<Json<SearchExplainResponse>> {
    let q = params.q.unwrap_or_default();
    let plan = parse_filter_query(&q);
    Ok(Json(SearchExplainResponse {
        query: q,
        plan: format!("{plan:?}"),
    }))
}

/// `GET /api/v1/messages/{id}`.
///
/// # Errors
/// `400` for an unparseable id, `404` if no such message exists.
#[utoipa::path(
    get, path = "/api/v1/messages/{id}", tag = "messages",
    params(("id" = String, Path, description = "Message id (UUID)")),
    responses(
        (status = 200, description = "The message with headers and attachments", body = MessageDetail),
        (status = 400, description = "Malformed request", body = crate::error::ProblemDetails),
        (status = 404, description = "No such entity", body = crate::error::ProblemDetails),
    )
)]
pub async fn get_message(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> ApiResult<Json<MessageDetail>> {
    let (message, entries) = load_message(&state, &id).await?;
    let names = mailbox_names(&state, &entries).await?;

    Ok(Json(MessageDetail {
        summary: MessageSummary::from_core(&message, entry_refs(&entries, &names)),
        headers: message
            .parsed
            .headers
            .iter()
            .map(HeaderDto::from_core)
            .collect(),
        attachments: message
            .parsed
            .attachments
            .iter()
            .map(AttachmentDto::from_core)
            .collect(),
        diagnostics: message
            .parsed
            .diagnostics
            .iter()
            .map(DiagnosticDto::from_core)
            .collect(),
        source: message.source.as_str().to_owned(),
    }))
}

/// `DELETE /api/v1/messages/{id}` — deletes a message and all of its placements across all mailboxes.
///
/// # Errors
/// Returns `400` if the id is not a UUID and `404` if the message does not exist.
#[utoipa::path(
    delete, path = "/api/v1/messages/{id}", tag = "messages",
    params(("id" = String, Path, description = "Message id (UUID)")),
    responses(
        (status = 204, description = "The message and all placements were deleted"),
        (status = 400, description = "Malformed request", body = crate::error::ProblemDetails),
        (status = 404, description = "No such entity", body = crate::error::ProblemDetails),
    )
)]
pub async fn delete_message(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    let message_id = MessageId::from_uuid(parse_id(&id, "message_id")?);
    let entries = state.messages.entries_for_message(message_id).await?;
    if entries.is_empty() && state.messages.get(message_id).await?.is_none() {
        return Err(ApiError::NotFound("Message"));
    }

    for entry in entries {
        state.messages.delete_entry(entry.id).await?;
    }
    state.messages.collect_orphans().await?;

    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/v1/messages/{id}/raw` — the exact original octets.
///
/// One of Sandesh's core correctness guarantees. The bytes are written to the
/// body untouched: no UTF-8 conversion, no line-ending normalisation, no
/// re-encoding. What SMTP received is what comes back.
///
/// # Errors
/// `400` for an unparseable id, `404` if no such message exists.
#[utoipa::path(
    get, path = "/api/v1/messages/{id}/raw", tag = "messages",
    params(("id" = String, Path, description = "Message id (UUID)")),
    responses(
        (status = 200, description = "The exact stored octets", content_type = "message/rfc822", body = String),
        (status = 400, description = "Malformed request", body = crate::error::ProblemDetails),
        (status = 404, description = "No such entity", body = crate::error::ProblemDetails),
    )
)]
pub async fn get_message_raw(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    let message_id = MessageId::from_uuid(parse_id(&id, "message_id")?);
    let raw = state
        .messages
        .raw(message_id)
        .await?
        .ok_or(ApiError::NotFound("Message"))?;

    let bytes = raw.bytes().to_vec();
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "message/rfc822"),
            (header::CONTENT_LENGTH, &bytes.len().to_string()),
        ],
        Body::from(bytes),
    )
        .into_response())
}

/// `GET /api/v1/messages/{id}/headers`.
///
/// # Errors
/// `400` for an unparseable id, `404` if no such message exists.
#[utoipa::path(
    get, path = "/api/v1/messages/{id}/headers", tag = "messages",
    params(("id" = String, Path, description = "Message id (UUID)")),
    responses(
        (status = 200, description = "Every header, in order, duplicates preserved", body = Vec<HeaderDto>),
        (status = 400, description = "Malformed request", body = crate::error::ProblemDetails),
        (status = 404, description = "No such entity", body = crate::error::ProblemDetails),
    )
)]
pub async fn get_message_headers(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<HeaderDto>>> {
    let (message, _) = load_message(&state, &id).await?;
    Ok(Json(
        message
            .parsed
            .headers
            .iter()
            .map(HeaderDto::from_core)
            .collect(),
    ))
}

/// `GET /api/v1/messages/{id}/headers/analysis`.
///
/// The interpreted view of the same headers `/headers` returns flat: the
/// `Received:` chain as a chronological hop list with per-hop delay, and the
/// authentication headers upstream left behind.
///
/// **Nothing here is verified.** The response carries a `disclaimer` field
/// stating so, because a client that renders `dkim=pass` without the caveat
/// would be claiming something Sandesh has not checked.
///
/// # Errors
/// `400` for an unparseable id, `404` if no such message exists.
#[utoipa::path(
    get, path = "/api/v1/messages/{id}/headers/analysis", tag = "messages",
    params(("id" = String, Path, description = "Message id (UUID)")),
    responses(
        (status = 200, description = "Parsed — not verified — header analysis", body = HeaderAnalysisDto),
        (status = 400, description = "Malformed request", body = crate::error::ProblemDetails),
        (status = 404, description = "No such entity", body = crate::error::ProblemDetails),
    )
)]
pub async fn get_message_header_analysis(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> ApiResult<Json<HeaderAnalysisDto>> {
    let (message, _) = load_message(&state, &id).await?;
    let analysis = sandesh_core::HeaderAnalysis::analyze(&message.parsed.headers);
    Ok(Json(HeaderAnalysisDto::from_core(&analysis)))
}

/// `GET /api/v1/messages/{id}/links`.
///
/// Every URL the message's HTML parts reference, classified by the role the
/// markup gives it, with the distinct hosts summarised.
///
/// **No URL here is fetched.** Extraction is entirely local (ADR-090):
/// a mail server that resolved the links in mail it received would be an
/// SSRF vector pointed at whatever a message chose, and would confirm to a
/// sender that their message was opened. The response carries a `disclaimer`
/// field saying so, for the same reason the header analysis does.
///
/// A message with no HTML part returns an empty report, not a `404` — "this
/// message references nothing" is an answer.
///
/// # Errors
/// `400` for an unparseable id, `404` if no such message exists.
#[utoipa::path(
    get, path = "/api/v1/messages/{id}/links", tag = "messages",
    params(("id" = String, Path, description = "Message id (UUID)")),
    responses(
        (status = 200, description = "Extracted — not resolved — links and images", body = LinkReportDto),
        (status = 400, description = "Malformed request", body = crate::error::ProblemDetails),
        (status = 404, description = "No such entity", body = crate::error::ProblemDetails),
    )
)]
pub async fn get_message_links(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> ApiResult<Json<LinkReportDto>> {
    let (message, _) = load_message(&state, &id).await?;

    let mut report = sandesh_core::LinkReport::default();
    for path in &message.parsed.html_parts {
        // Decoded, not raw: a quoted-printable part writes `=3D` for every `=`
        // in a query string, so scanning the stored bytes would report URLs
        // the message never contained.
        if let Some(body) = message.part_body_decoded(path) {
            report.merge(sandesh_core::LinkReport::extract(&body, &path.to_string()));
        }
    }

    Ok(Json(LinkReportDto::from_core(&report)))
}

/// `GET /api/v1/messages/{id}/mime`.
///
/// Parts are reported as byte ranges into the raw message, which is how they
/// are stored (`CORE_SPEC.md`). There is no second MIME representation here —
/// the API projects the one the core already has.
///
/// # Errors
/// `400` for an unparseable id, `404` if no such message exists.
#[utoipa::path(
    get, path = "/api/v1/messages/{id}/mime", tag = "messages",
    params(("id" = String, Path, description = "Message id (UUID)")),
    responses(
        (status = 200, description = "The MIME tree with raw byte ranges", body = MimeTree),
        (status = 400, description = "Malformed request", body = crate::error::ProblemDetails),
        (status = 404, description = "No such entity", body = crate::error::ProblemDetails),
    )
)]
pub async fn get_message_mime(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> ApiResult<Json<MimeTree>> {
    let (message, _) = load_message(&state, &id).await?;

    Ok(Json(MimeTree {
        parts: message
            .parsed
            .parts
            .iter()
            .map(MimePartDto::from_core)
            .collect(),
        text_parts: message
            .parsed
            .text_parts
            .iter()
            .map(ToString::to_string)
            .collect(),
        html_parts: message
            .parsed
            .html_parts
            .iter()
            .map(ToString::to_string)
            .collect(),
        parse_ok: message.parsed.parse_ok,
        diagnostics: message
            .parsed
            .diagnostics
            .iter()
            .map(DiagnosticDto::from_core)
            .collect(),
    }))
}

/// Query parameters for fetching a MIME part.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PartQuery {
    /// Return the raw, undecoded bytes as stored in the message, instead of the
    /// decoded content. Defaults to `false`.
    #[serde(default)]
    pub raw: bool,
}

/// `GET /api/v1/messages/{id}/part/{path}` — one decoded MIME part.
///
/// Returns the decoded body (transfer encodings such as Base64 and Quoted-Printable
/// decoded). Set `?raw=true` to retrieve the verbatim encoded bytes as stored.
///
/// # Errors
/// `400` for an unparseable id or part path, `404` if no such message or part exists.
#[utoipa::path(
    get, path = "/api/v1/messages/{id}/part/{path}", tag = "messages",
    params(
        ("id" = String, Path, description = "Message id (UUID)"),
        ("path" = String, Path, description = "Dotted MIME part path; `root` for the top part"),
        PartQuery,
    ),
    responses(
        (status = 200, description = "The decoded part body", body = String),
        (status = 400, description = "Malformed request", body = crate::error::ProblemDetails),
        (status = 404, description = "No such entity", body = crate::error::ProblemDetails),
    )
)]
pub async fn get_message_part(
    State(state): State<ApiState>,
    Path((id, path)): Path<(String, String)>,
    Query(query): Query<PartQuery>,
) -> ApiResult<Response> {
    let (message, _) = load_message(&state, &id).await?;

    // The domain renders the root part as the empty string, which no URL path
    // segment can carry. `root` is the URL spelling for it; every other path is
    // the ordinary dotted form. Without this the root part of a non-multipart
    // message — the common case — would be unaddressable.
    let part_path = if path == ROOT_PART_PATH {
        sandesh_core::PartPath::root()
    } else {
        sandesh_core::PartPath::parse(&path)
            .ok_or_else(|| ApiError::bad_request(format!("'{path}' is not a MIME part path")))?
    };
    let part: &MimePart = message
        .parsed
        .part(&part_path)
        .ok_or(ApiError::NotFound("MIME part"))?;

    let body = if query.raw {
        message
            .part_body(&part_path)
            .ok_or(ApiError::NotFound("MIME part"))?
            .to_vec()
    } else {
        message
            .part_body_decoded(&part_path)
            .ok_or(ApiError::NotFound("MIME part"))?
    };

    // The declared content type is echoed back, but never as a type a browser
    // will execute in Sandesh's own origin: an attacker-supplied HTML part
    // rendered inline could act on behalf of whoever is viewing it.
    let content_type = if is_renderable_inline(&part.content_type) {
        part.content_type.clone()
    } else {
        String::from("application/octet-stream")
    };

    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, content_type),
            (header::CONTENT_DISPOSITION, String::from("attachment")),
        ],
        Body::from(body),
    )
        .into_response())
}

/// `GET /api/v1/messages/{id}/delivery`.
///
/// # Errors
/// `400` for an unparseable id, `404` if no delivery record names this message.
#[utoipa::path(
    get, path = "/api/v1/messages/{id}/delivery", tag = "messages",
    params(("id" = String, Path, description = "Message id (UUID)")),
    responses(
        (status = 200, description = "How the message reached Sandesh", body = DeliveryDto),
        (status = 400, description = "Malformed request", body = crate::error::ProblemDetails),
        (status = 404, description = "No such entity", body = crate::error::ProblemDetails),
    )
)]
pub async fn get_message_delivery(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> ApiResult<Json<DeliveryDto>> {
    let message_id = MessageId::from_uuid(parse_id(&id, "message_id")?);
    let delivery = state
        .deliveries
        .by_message_id(message_id)
        .await?
        .ok_or(ApiError::NotFound("Delivery"))?;

    Ok(Json(delivery_dto(&delivery)))
}

/// `GET /api/v1/deliveries` — recent deliveries, newest first.
///
/// The counterpart to `/messages/{id}/delivery`, and not merely a convenience:
/// a transaction rejected outright stores no message, so its record has no
/// message id to look it up by and is reachable *only* here. Without this
/// endpoint the rejection records would be written and then invisible, which
/// is worse than not writing them.
///
/// # Errors
/// `400` if `limit` is out of range.
#[utoipa::path(
    get, path = "/api/v1/deliveries", tag = "deliveries",
    params(("limit" = Option<u32>, Query, description = "Page size, capped at MAX_LIMIT")),
    responses(
        (status = 200, description = "Recent deliveries", body = ListResponseDeliveryDto),
        (status = 400, description = "Malformed request", body = crate::error::ProblemDetails),
    )
)]
pub async fn list_deliveries(
    State(state): State<ApiState>,
    Query(pagination): Query<Pagination>,
) -> ApiResult<Json<ListResponse<DeliveryDto>>> {
    let limit = pagination.limit()?;
    let deliveries = state.deliveries.recent(limit).await?;

    let items: Vec<DeliveryDto> = deliveries.iter().map(delivery_dto).collect();
    let total = items.len() as u64;

    Ok(Json(ListResponse {
        items,
        // Deliveries are a bounded diagnostic tail, not a browsable archive:
        // `DeliveryStore::recent` takes a limit and offers no cursor, so
        // advertising one would be a promise the store cannot keep.
        next_cursor: None,
        total_estimate: total,
    }))
}

/// Filtering for the connection listing.
#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ConnectionQuery {
    /// Page size. Defaults and caps as elsewhere.
    pub limit: Option<u32>,
    /// Restrict to one protocol: `smtp`, `imap` or `pop3`.
    pub protocol: Option<String>,
}

/// `GET /api/v1/connections` — recent connection transcripts, newest first.
///
/// The retrieval half of Phase 7 A2. Transcripts have been captured and stored
/// since ADR-066 with no way to read them back, so the phase's criterion —
/// "every connection to every protocol produces a **retrievable** trace" — was
/// met on the capture side and unmet end to end.
///
/// `?protocol=smtp|imap|pop3` filters; omitting it returns every protocol
/// interleaved, which is what "what just happened?" usually means.
///
/// # Errors
/// `400` if `limit` or `protocol` is unusable, `503` when tracing is disabled.
#[utoipa::path(
    get, path = "/api/v1/connections", tag = "connections",
    params(
        ("limit" = Option<u32>, Query, description = "Page size, capped at MAX_LIMIT"),
        ("protocol" = Option<String>, Query, description = "Filter by smtp, imap or pop3"),
    ),
    responses(
        (status = 200, description = "Recent connections", body = ListResponseConnectionDto),
        (status = 400, description = "Malformed request", body = crate::error::ProblemDetails),
        (status = 503, description = "Tracing is disabled", body = crate::error::ProblemDetails),
    )
)]
pub async fn list_connections(
    State(state): State<ApiState>,
    Query(params): Query<ConnectionQuery>,
) -> ApiResult<Json<ListResponse<ConnectionDto>>> {
    let store = connection_store(&state)?;
    let limit = Pagination {
        limit: params.limit,
        ..Pagination::default()
    }
    .limit()?;

    // An unrecognised protocol is a client error, not an empty result: silently
    // returning nothing for `?protocol=smtpp` would look like "no SMTP
    // connections yet" to someone who simply mistyped.
    let protocol = match params.protocol.as_deref() {
        None => None,
        Some(raw) => Some(sandesh_core::Protocol::parse(raw).ok_or_else(|| {
            ApiError::bad_request(format!("protocol must be smtp, imap or pop3; got `{raw}`"))
        })?),
    };

    let connections = store.recent(protocol, limit).await?;
    let items: Vec<ConnectionDto> = connections.iter().map(connection_dto).collect();
    let total = items.len() as u64;

    Ok(Json(ListResponse {
        items,
        // Connections are a bounded diagnostic tail, not a browsable archive:
        // `ConnectionStore::recent` takes a limit and offers no cursor, so
        // advertising one would be a promise the store cannot keep. The same
        // reasoning as `/deliveries`.
        next_cursor: None,
        total_estimate: total,
    }))
}

/// `GET /api/v1/connections/{id}` — one connection's full transcript.
///
/// # Errors
/// `400` for a malformed id, `404` if no such record, `503` when tracing is
/// disabled.
#[utoipa::path(
    get, path = "/api/v1/connections/{id}", tag = "connections",
    params(("id" = String, Path, description = "Connection identifier")),
    responses(
        (status = 200, description = "The connection", body = ConnectionDto),
        (status = 404, description = "No such connection", body = crate::error::ProblemDetails),
        (status = 503, description = "Tracing is disabled", body = crate::error::ProblemDetails),
    )
)]
pub async fn get_connection(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> ApiResult<Json<ConnectionDto>> {
    let store = connection_store(&state)?;
    let connection_id = sandesh_core::ConnectionId::from_uuid(parse_id(&id, "connection_id")?);
    let connection = store
        .get(connection_id)
        .await?
        .ok_or(ApiError::NotFound("Connection"))?;

    Ok(Json(connection_dto(&connection)))
}

/// The connection store, or a `503` explaining that tracing is off.
///
/// Distinguishing "tracing is disabled" from "nothing has connected yet"
/// matters: both would otherwise be an empty list, and the person reading it is
/// usually asking why their trace is missing. `503` with the reason answers
/// that; `200 []` sends them looking for a bug in the capture path.
fn connection_store(
    state: &ApiState,
) -> ApiResult<&std::sync::Arc<dyn sandesh_core::ConnectionStore>> {
    state.connections.as_ref().ok_or_else(|| {
        ApiError::Unavailable(String::from(
            "connection tracing is disabled; set SANDESH_TRACING_ENABLED=true to record transcripts",
        ))
    })
}

/// Maps a connection record to its wire shape.
///
/// Shared by the listing and the single lookup, so the two cannot drift into
/// describing the same record differently — the same reason `delivery_dto`
/// exists.
fn connection_dto(connection: &sandesh_core::ConnectionRecord) -> ConnectionDto {
    ConnectionDto {
        id: connection.id.to_string(),
        protocol: String::from(connection.protocol.as_str()),
        peer: connection.peer.clone(),
        encrypted: connection.encrypted,
        opened_at: rfc3339(connection.opened_at),
        closed_at: connection.closed_at.map(rfc3339),
        disposition: connection.disposition.map(|d| String::from(d.as_str())),
        // Computed rather than stored: the two timestamps are the record, and a
        // derived field that disagreed with them would be worse than absent.
        duration_us: connection
            .closed_at
            .map(|closed| closed.as_micros() - connection.opened_at.as_micros()),
        transcript: connection.transcript.clone(),
        truncated: connection.truncated,
    }
}

/// Maps a delivery record to its wire shape.
///
/// Shared by the single-message lookup and the delivery listing, so the two can
/// never drift into describing the same record differently.
fn delivery_dto(delivery: &Delivery) -> DeliveryDto {
    DeliveryDto {
        id: delivery.id.to_string(),
        message_id: delivery.message_id.map(|id| id.to_string()),
        mail_from: delivery
            .envelope
            .mail_from
            .as_ref()
            .map(EmailAddress::normalized),
        rcpt_to: delivery
            .envelope
            .rcpt_to
            .iter()
            .map(EmailAddress::normalized)
            .collect(),
        source: delivery.source.as_str().to_owned(),
        peer: delivery.envelope.peer.clone(),
        helo: delivery.envelope.helo_domain.clone(),
        received_at: rfc3339(delivery.received_at),
        duration_us: delivery.duration_us,
        recipients: delivery
            .outcomes
            .iter()
            .map(|recipient| RecipientOutcomeDto {
                address: recipient.rcpt_to.normalized(),
                // The domain's own stable name, not Debug output.
                outcome: recipient.outcome.as_str().to_owned(),
                reason: recipient.reason.clone(),
                resolved_account_id: recipient.resolved_account.map(|id| id.to_string()),
                entry_id: recipient.entry_id.map(|id| id.to_string()),
            })
            .collect(),
        replay_of: delivery.replay_of.map(|id| id.to_string()),
        rules_applied: delivery
            .rules_applied
            .iter()
            .cloned()
            .map(Into::into)
            .collect(),
    }
}

/// Query parameters for fetching an attachment.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct AttachmentQuery {
    /// Return the raw, undecoded bytes as stored in the message, instead of the
    /// decoded content. Defaults to `false`.
    #[serde(default)]
    pub raw: bool,
    /// Serve with `Content-Disposition: inline` for browser viewing/previewing,
    /// provided the MIME type is safe to display. Defaults to `false`.
    #[serde(default)]
    pub inline: bool,
}

/// `GET /api/v1/messages/{id}/attachments` — metadata for all attachments of a message.
///
/// # Errors
/// `400` for an unparseable id, `404` if the message does not exist.
#[utoipa::path(
    get, path = "/api/v1/messages/{id}/attachments", tag = "messages",
    params(("id" = String, Path, description = "Message id (UUID)")),
    responses(
        (status = 200, description = "Metadata list of attachments", body = ListResponseAttachmentDto),
        (status = 400, description = "Malformed request", body = crate::error::ProblemDetails),
        (status = 404, description = "No such entity", body = crate::error::ProblemDetails),
    )
)]
pub async fn list_message_attachments(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> ApiResult<Json<ListResponse<AttachmentDto>>> {
    let (message, _) = load_message(&state, &id).await?;
    let attachments = state.messages.attachments(message.id).await?;
    let items: Vec<AttachmentDto> = attachments.iter().map(AttachmentDto::from_core).collect();
    let total = items.len() as u64;
    Ok(Json(ListResponse {
        items,
        next_cursor: None,
        total_estimate: total,
    }))
}

/// `GET /api/v1/attachments/{id}` — download or preview decoded attachment bytes.
///
/// # Errors
/// `400` for an unparseable id, `404` if the attachment does not exist.
#[utoipa::path(
    get, path = "/api/v1/attachments/{id}", tag = "attachments",
    params(
        ("id" = String, Path, description = "Attachment id (UUID)"),
        AttachmentQuery,
    ),
    responses(
        (status = 200, description = "Attachment content bytes", body = Vec<u8>),
        (status = 400, description = "Malformed request", body = crate::error::ProblemDetails),
        (status = 404, description = "No such entity", body = crate::error::ProblemDetails),
    )
)]
pub async fn get_attachment(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Query(query): Query<AttachmentQuery>,
) -> ApiResult<Response> {
    let attachment_id = AttachmentId::from_uuid(parse_id(&id, "attachment_id")?);
    let (attachment, message_id) = state
        .messages
        .get_attachment(attachment_id)
        .await?
        .ok_or(ApiError::NotFound("Attachment"))?;

    let message = state
        .messages
        .get(message_id)
        .await?
        .ok_or(ApiError::NotFound("Message"))?;

    let body = if query.raw {
        message
            .part_body(&attachment.part_path)
            .ok_or(ApiError::NotFound("Attachment content"))?
            .to_vec()
    } else {
        message
            .part_body_decoded(&attachment.part_path)
            .ok_or(ApiError::NotFound("Attachment content"))?
    };

    let sanitized_filename = attachment
        .filename
        .as_deref()
        .unwrap_or("attachment")
        .replace(['\r', '\n', '"', '\\'], "_");

    let is_safe = is_renderable_inline(&attachment.content_type);
    let (content_type, disposition) = if query.inline && is_safe {
        (
            attachment.content_type.clone(),
            format!("inline; filename=\"{sanitized_filename}\""),
        )
    } else {
        let ct = if is_safe {
            attachment.content_type.clone()
        } else {
            let base = attachment
                .content_type
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase();
            if base == "text/html" || base == "image/svg+xml" {
                String::from("application/octet-stream")
            } else {
                attachment.content_type.clone()
            }
        };
        (ct, format!("attachment; filename=\"{sanitized_filename}\""))
    };

    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, content_type),
            (header::CONTENT_DISPOSITION, disposition),
            (header::X_CONTENT_TYPE_OPTIONS, String::from("nosniff")),
        ],
        Body::from(body),
    )
        .into_response())
}

/// `GET /api/v1/attachments/{id}/thumbnail` — thumbnail for an image attachment.
///
/// Returns 404 for non-image attachments per `API_DESIGN.md` §3.5.
///
/// # Errors
/// `400` for an unparseable id or unreadable image, `404` if the attachment or thumbnail does not exist.
#[utoipa::path(
    get, path = "/api/v1/attachments/{id}/thumbnail", tag = "attachments",
    params(("id" = String, Path, description = "Attachment id (UUID)")),
    responses(
        (status = 200, description = "Thumbnail image bytes (PNG)", body = Vec<u8>),
        (status = 400, description = "Malformed image", body = crate::error::ProblemDetails),
        (status = 404, description = "Not found or not an image", body = crate::error::ProblemDetails),
    )
)]
pub async fn get_attachment_thumbnail(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    let attachment_id = AttachmentId::from_uuid(parse_id(&id, "attachment_id")?);
    let (attachment, message_id) = state
        .messages
        .get_attachment(attachment_id)
        .await?
        .ok_or(ApiError::NotFound("Attachment"))?;

    let base_type = attachment
        .content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();

    if !matches!(
        base_type.as_str(),
        "image/png" | "image/jpeg" | "image/jpg" | "image/webp" | "image/gif"
    ) {
        return Err(ApiError::NotFound(
            "Image thumbnail (attachment is not a supported image format)",
        ));
    }

    let message = state
        .messages
        .get(message_id)
        .await?
        .ok_or(ApiError::NotFound("Message"))?;

    let body = message
        .part_body_decoded(&attachment.part_path)
        .ok_or(ApiError::NotFound("Attachment content"))?;

    let img = image::load_from_memory(&body)
        .map_err(|e| ApiError::bad_request(format!("Cannot decode image: {e}")))?;

    let thumb = img.thumbnail(200, 200);
    let mut thumb_bytes = std::io::Cursor::new(Vec::new());
    thumb
        .write_to(&mut thumb_bytes, image::ImageFormat::Png)
        .map_err(|e| ApiError::Internal(format!("Failed to encode thumbnail: {e}")))?;

    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, String::from("image/png")),
            (
                header::CONTENT_DISPOSITION,
                String::from("inline; filename=\"thumbnail.png\""),
            ),
            (
                header::CACHE_CONTROL,
                String::from("public, max-age=86400, immutable"),
            ),
            (header::X_CONTENT_TYPE_OPTIONS, String::from("nosniff")),
        ],
        Body::from(thumb_bytes.into_inner()),
    )
        .into_response())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Whether a content type is safe to serve with its own type.
///
/// Deliberately a small allow-list. Anything else — HTML and SVG in particular,
/// both of which execute script — is served as `application/octet-stream`.
fn is_renderable_inline(content_type: &str) -> bool {
    let base = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    matches!(
        base.as_str(),
        "text/plain" | "image/png" | "image/jpeg" | "image/gif" | "image/webp" | "application/pdf"
    )
}

/// Parses a UUID path or query parameter.
///
/// Every identifier that arrives over HTTP passes through here. An unparseable
/// id is a `400` naming the field, not a `500` and not a database lookup for a
/// value that could never match.
fn parse_id(raw: &str, field: &str) -> ApiResult<uuid::Uuid> {
    uuid::Uuid::parse_str(raw)
        .map_err(|_| ApiError::bad_request(format!("{field} is not a valid identifier")))
}

/// Splits flag names into system flags and keywords.
fn parse_flags(names: &[String]) -> ApiResult<(SystemFlags, BTreeSet<Keyword>)> {
    let mut system = SystemFlags::NONE;
    let mut keywords = BTreeSet::new();

    for name in names {
        if let Some(flag) = SystemFlags::from_name(name) {
            system = system.union(flag);
        } else if let Some(keyword) = Keyword::new(name.clone()) {
            keywords.insert(keyword);
        } else {
            return Err(ApiError::Unprocessable(format!(
                "'{name}' is not a usable flag name"
            )));
        }
    }
    Ok((system, keywords))
}

/// Loads a message and its placements, or reports `404`.
async fn load_message(state: &ApiState, id: &str) -> ApiResult<(Message, Vec<MessageEntry>)> {
    let message_id = MessageId::from_uuid(parse_id(id, "message_id")?);
    let message = state
        .messages
        .get(message_id)
        .await?
        .ok_or(ApiError::NotFound("Message"))?;
    let entries = state.messages.entries_for_message(message_id).await?;
    Ok((message, entries))
}

/// The mailbox names for a set of entries, so a summary can name them.
async fn mailbox_names(
    state: &ApiState,
    entries: &[MessageEntry],
) -> ApiResult<BTreeMap<MailboxId, String>> {
    let mut names = BTreeMap::new();
    for entry in entries {
        if names.contains_key(&entry.mailbox_id) {
            continue;
        }
        if let Some(mailbox) = state.mailboxes.get(entry.mailbox_id).await? {
            names.insert(entry.mailbox_id, mailbox.name.as_str().to_owned());
        }
    }
    Ok(names)
}

/// Builds the entry references carried by a summary.
fn entry_refs(
    entries: &[MessageEntry],
    names: &BTreeMap<MailboxId, String>,
) -> Vec<crate::dto::EntryRef> {
    entries
        .iter()
        .map(|entry| crate::dto::EntryRef::from_core(entry, names.get(&entry.mailbox_id).cloned()))
        .collect()
}

/// Which mailboxes a message query covers.
async fn scope_mailboxes(state: &ApiState, page: &Pagination) -> ApiResult<Vec<MailboxId>> {
    if let Some(raw) = &page.mailbox_id {
        return Ok(vec![MailboxId::from_uuid(parse_id(raw, "mailbox_id")?)]);
    }

    let accounts = if let Some(raw) = &page.account_id {
        vec![sandesh_core::AccountId::from_uuid(parse_id(
            raw,
            "account_id",
        )?)]
    } else {
        state
            .accounts
            .list()
            .await?
            .into_iter()
            .map(|account| account.id)
            .collect()
    };

    let mut mailboxes = Vec::new();
    for account_id in accounts {
        mailboxes.extend(
            state
                .mailboxes
                .list(account_id)
                .await?
                .into_iter()
                .map(|mailbox| mailbox.id),
        );
    }
    Ok(mailboxes)
}

/// `POST /api/v1/send` — compose and deliver a message.
///
/// Accepts a structured message request, synthesizes an RFC 5322 MIME message,
/// resolves recipients to their respective `INBOX` mailboxes, places a copy
/// into the sender's `Sent` mailbox, and persists the message through
/// [`MessageStore`].
///
/// # Errors
/// `400` if recipient list is empty or from address is missing.
#[utoipa::path(
    post, path = "/api/v1/send", tag = "messages",
    request_body = SendMessageRequest,
    responses(
        (status = 201, description = "Message accepted and delivered", body = SendMessageResponse),
        (status = 400, description = "Malformed request", body = crate::error::ProblemDetails),
        (status = 422, description = "Unprocessable address or payload", body = crate::error::ProblemDetails),
    )
)]
#[allow(clippy::too_many_lines)]
pub async fn send_message(
    State(state): State<ApiState>,
    Json(req): Json<SendMessageRequest>,
) -> ApiResult<(StatusCode, Json<SendMessageResponse>)> {
    if !req.is_draft.unwrap_or(false) && req.to.is_empty() {
        return Err(ApiError::bad_request(
            "at least one recipient is required in 'to'",
        ));
    }

    let sender_addr = EmailAddress::parse_lenient(&req.from);
    if sender_addr.original().trim().is_empty() {
        return Err(ApiError::bad_request("'from' address cannot be empty"));
    }

    let id = MessageId::new();
    let rfc_message_id = format!("<{id}@sandesh.local>");
    let now = Timestamp::now();

    let date_str = now
        .to_datetime()
        .and_then(|dt| {
            dt.format(&time::format_description::well_known::Rfc2822)
                .ok()
        })
        .unwrap_or_else(|| String::from("Thu, 01 Jan 1970 00:00:00 +0000"));

    let normalize_crlf = |s: &str| -> String { s.replace("\r\n", "\n").replace('\n', "\r\n") };

    let mut raw_bytes = Vec::new();
    raw_bytes.extend_from_slice(format!("From: {}\r\n", req.from.trim()).as_bytes());
    raw_bytes.extend_from_slice(format!("To: {}\r\n", req.to.join(", ")).as_bytes());

    if !req.cc.is_empty() {
        raw_bytes.extend_from_slice(format!("Cc: {}\r\n", req.cc.join(", ")).as_bytes());
    }

    raw_bytes.extend_from_slice(format!("Subject: {}\r\n", req.subject.trim()).as_bytes());
    raw_bytes.extend_from_slice(format!("Date: {date_str}\r\n").as_bytes());
    raw_bytes.extend_from_slice(format!("Message-ID: {rfc_message_id}\r\n").as_bytes());

    let format_msg_id = |s: &str| -> String {
        let trimmed = s.trim();
        if trimmed.starts_with('<') && trimmed.ends_with('>') {
            trimmed.to_string()
        } else {
            format!("<{trimmed}>")
        }
    };

    let mut formatted_in_reply_to = None;
    if let Some(in_reply_to) = &req.in_reply_to {
        let trimmed = in_reply_to.trim();
        if !trimmed.is_empty() {
            let formatted = format_msg_id(trimmed);
            raw_bytes.extend_from_slice(format!("In-Reply-To: {formatted}\r\n").as_bytes());
            formatted_in_reply_to = Some(formatted);
        }
    }

    if let Some(references) = &req.references {
        let trimmed = references.trim();
        if !trimmed.is_empty() {
            let formatted_refs: Vec<String> =
                trimmed.split_whitespace().map(format_msg_id).collect();
            raw_bytes.extend_from_slice(
                format!("References: {}\r\n", formatted_refs.join(" ")).as_bytes(),
            );
        } else if let Some(parent_id) = &formatted_in_reply_to {
            raw_bytes.extend_from_slice(format!("References: {parent_id}\r\n").as_bytes());
        }
    } else if let Some(parent_id) = &formatted_in_reply_to {
        raw_bytes.extend_from_slice(format!("References: {parent_id}\r\n").as_bytes());
    }

    raw_bytes.extend_from_slice(b"MIME-Version: 1.0\r\n");

    match (&req.text_body, &req.html_body) {
        (Some(text), Some(html)) if !html.is_empty() => {
            let boundary = format!("----=_Sandesh_Part_{id}");
            raw_bytes.extend_from_slice(
                format!("Content-Type: multipart/alternative; boundary=\"{boundary}\"\r\n\r\n")
                    .as_bytes(),
            );
            raw_bytes.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
            raw_bytes.extend_from_slice(b"Content-Type: text/plain; charset=utf-8\r\n");
            raw_bytes.extend_from_slice(b"Content-Transfer-Encoding: 8bit\r\n\r\n");
            raw_bytes.extend_from_slice(normalize_crlf(text).as_bytes());
            raw_bytes.extend_from_slice(b"\r\n");

            raw_bytes.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
            raw_bytes.extend_from_slice(b"Content-Type: text/html; charset=utf-8\r\n");
            raw_bytes.extend_from_slice(b"Content-Transfer-Encoding: 8bit\r\n\r\n");
            raw_bytes.extend_from_slice(normalize_crlf(html).as_bytes());
            raw_bytes.extend_from_slice(b"\r\n");

            raw_bytes.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
        }
        (None, Some(html)) if !html.is_empty() => {
            raw_bytes.extend_from_slice(b"Content-Type: text/html; charset=utf-8\r\n");
            raw_bytes.extend_from_slice(b"Content-Transfer-Encoding: 8bit\r\n\r\n");
            raw_bytes.extend_from_slice(normalize_crlf(html).as_bytes());
            raw_bytes.extend_from_slice(b"\r\n");
        }
        (Some(text), _) => {
            raw_bytes.extend_from_slice(b"Content-Type: text/plain; charset=utf-8\r\n");
            raw_bytes.extend_from_slice(b"Content-Transfer-Encoding: 8bit\r\n\r\n");
            raw_bytes.extend_from_slice(normalize_crlf(text).as_bytes());
            raw_bytes.extend_from_slice(b"\r\n");
        }
        _ => {
            raw_bytes.extend_from_slice(b"Content-Type: text/plain; charset=utf-8\r\n");
            raw_bytes.extend_from_slice(b"Content-Transfer-Encoding: 8bit\r\n\r\n\r\n");
        }
    }

    let raw = RawMessage::new(raw_bytes);
    let parsed = sandesh_core::parse(raw.bytes());

    let is_draft = req.is_draft.unwrap_or(false);
    let mut placements = Vec::new();

    let mut all_recipients = Vec::new();
    for to in &req.to {
        all_recipients.push(EmailAddress::parse_lenient(to));
    }
    for cc in &req.cc {
        all_recipients.push(EmailAddress::parse_lenient(cc));
    }
    for bcc in &req.bcc {
        all_recipients.push(EmailAddress::parse_lenient(bcc));
    }

    let sender_acc_id = match state.accounts.resolve_address(&sender_addr).await {
        Ok(Some(id)) => Some(id),
        _ => match state.accounts.list().await {
            Ok(mut list) if !list.is_empty() => Some(list.remove(0).id),
            _ => None,
        },
    };

    if is_draft {
        let drafts_name =
            MailboxName::new("Drafts").map_err(|e| ApiError::Internal(e.to_string()))?;
        if let Some(acc_id) = sender_acc_id
            && let Ok(Some(drafts_mbx)) = state.mailboxes.get_by_name(acc_id, &drafts_name).await
        {
            placements.push(NewPlacement {
                mailbox_id: drafts_mbx.id,
                flags: MessageFlags::from_system(SystemFlags::DRAFT.union(SystemFlags::SEEN)),
                internal_date: now,
            });
        }
    } else {
        let inbox_name = MailboxName::new(INBOX).map_err(|e| ApiError::Internal(e.to_string()))?;

        for addr in &all_recipients {
            if let Ok(Some(account_id)) = state.accounts.resolve_address(addr).await
                && let Ok(Some(inbox)) = state.mailboxes.get_by_name(account_id, &inbox_name).await
                && !placements
                    .iter()
                    .any(|p: &NewPlacement| p.mailbox_id == inbox.id)
            {
                placements.push(NewPlacement {
                    mailbox_id: inbox.id,
                    flags: MessageFlags::empty(),
                    internal_date: now,
                });
            }
        }

        if placements.is_empty() {
            let fallback_acc_id = match state.accounts.catch_all().await {
                Ok(Some(catch_all)) => Some(catch_all.id),
                _ => match state.accounts.list().await {
                    Ok(mut list) if !list.is_empty() => Some(list.remove(0).id),
                    _ => None,
                },
            };

            if let Some(acc_id) = fallback_acc_id
                && let Ok(Some(inbox)) = state.mailboxes.get_by_name(acc_id, &inbox_name).await
            {
                placements.push(NewPlacement {
                    mailbox_id: inbox.id,
                    flags: MessageFlags::empty(),
                    internal_date: now,
                });
            }
        }

        let sent_name = MailboxName::new("Sent").map_err(|e| ApiError::Internal(e.to_string()))?;

        if let Some(acc_id) = sender_acc_id
            && let Ok(Some(sent_mbx)) = state.mailboxes.get_by_name(acc_id, &sent_name).await
            && !placements
                .iter()
                .any(|p: &NewPlacement| p.mailbox_id == sent_mbx.id)
        {
            placements.push(NewPlacement {
                mailbox_id: sent_mbx.id,
                flags: MessageFlags::from_system(SystemFlags::SEEN),
                internal_date: now,
            });
        }
    }

    let envelope = Envelope {
        mail_from: Some(sender_addr),
        rcpt_to: all_recipients,
        helo_domain: Some(String::from("localhost")),
        peer: Some(String::from("127.0.0.1:api")),
        tls: None,
        auth_identity: None,
    };

    let new_message = NewMessage {
        raw,
        envelope,
        source: DeliverySource::Api,
        parsed,
        received_at: now,
        placements,
    };

    let insert_result = state.messages.insert(&new_message).await?;

    let response = SendMessageResponse {
        id: insert_result.message_id.to_string(),
        message_id: rfc_message_id,
        subject: req.subject,
        placements: insert_result.entries.len(),
        created_at: now
            .to_datetime()
            .and_then(|dt| {
                dt.format(&time::format_description::well_known::Rfc3339)
                    .ok()
            })
            .unwrap_or_else(|| String::from("1970-01-01T00:00:00Z")),
    };

    Ok((StatusCode::CREATED, Json(response)))
}

/// `GET /api/v1/events/sse` — Server-Sent Events (SSE) live event stream.
#[utoipa::path(
    get, path = "/api/v1/events/sse", tag = "events",
    responses(
        (status = 200, description = "Event stream connection established", content_type = "text/event-stream")
    )
)]
pub async fn events_sse(
    State(state): State<ApiState>,
) -> Sse<impl futures_util::stream::Stream<Item = Result<SseEvent, std::convert::Infallible>>> {
    let rx = state.events.subscribe();
    let stream = futures_util::stream::unfold((rx, 1u64), |(mut rx, mut seq)| async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    let frame = crate::ws::event_frame(&event, seq);
                    let sse_event = SseEvent::default()
                        .event(event.kind().as_str())
                        .id(seq.to_string())
                        .data(frame.to_string());
                    seq += 1;
                    return Some((Ok(sse_event), (rx, seq)));
                }
                Err(sandesh_events::RecvError::Lagged(_)) => {}
                Err(sandesh_events::RecvError::Closed) => return None,
            }
        }
    });

    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// `GET /metrics` — the Prometheus scrape (Phase 7 A4).
///
/// Outside `/api/v1` because it is an operational surface with its own
/// contract and lifecycle, and a bare `/metrics` is what every scraper
/// defaults to. It is *not* exempt from authentication: `/info` and
/// `/health/ready` are already protected because they disclose deployment
/// state (ADR-075), and a scrape discloses more than either — message counts,
/// mailbox counts and delivery volumes. Prometheus carries a bearer token in
/// its scrape configuration, so protecting it costs an operator one line.
///
/// # Errors
/// Returns an error when the readings cannot be gathered — in practice, when
/// the database is unreachable.
pub async fn metrics(State(state): State<ApiState>) -> ApiResult<Response> {
    let snapshot = state.metrics.snapshot().await?;

    Ok((
        [(
            axum::http::header::CONTENT_TYPE,
            crate::metrics::CONTENT_TYPE,
        )],
        crate::metrics::render(&snapshot),
    )
        .into_response())
}

/// Query parameters for the log endpoints.
#[derive(Debug, Deserialize, IntoParams)]
pub struct LogQuery {
    /// Minimum severity, inclusive: `error`, `warn`, `info`, `debug`, `trace`.
    ///
    /// "at or above", because that is what a reader means by asking for
    /// warnings — not "warnings but not errors".
    pub level: Option<String>,
    /// Substring match against the emitting module path.
    pub target: Option<String>,
    /// How many lines to return, from the newest end.
    pub limit: Option<usize>,
}

/// `GET /api/v1/logs` — the recent backlog (Phase 7 A6).
///
/// Served alongside the stream so a page load shows history rather than an
/// empty pane that fills only when something next happens.
///
/// # Errors
/// None: an empty buffer is an empty list, not a failure.
#[utoipa::path(
    get,
    path = "/api/v1/logs",
    tag = "logs",
    params(LogQuery),
    responses((status = 200, description = "Recent log lines, oldest first")),
)]
pub async fn list_logs(
    State(state): State<ApiState>,
    Query(query): Query<LogQuery>,
) -> ApiResult<Json<ListResponse<LogLineDto>>> {
    let limit = query.limit.unwrap_or(200).min(MAX_LOG_LIMIT);

    let items: Vec<LogLineDto> = state
        .logs
        .recent(limit, query.level.as_deref(), query.target.as_deref())
        .into_iter()
        .map(LogLineDto::from)
        .collect();

    Ok(Json(ListResponse {
        // Exact rather than estimated: the ring's size is known, unlike a
        // table's under continuous insertion. There is no cursor because the
        // buffer is bounded — the whole of it fits in one response.
        total_estimate: items.len() as u64,
        items,
        next_cursor: None,
    }))
}

/// `GET /api/v1/logs/stream` — the live log, as Server-Sent Events.
///
/// SSE rather than a `WebSocket`: the stream is one-directional, so the
/// client never needs to send anything, and this mirrors the existing
/// `/events/sse` mirror rather than inventing a second `WebSocket` surface.
///
/// A lagging consumer is skipped past rather than disconnected. Dropping the
/// oldest lines matches the event bus's stance (`sandesh-events`): a reader
/// this far behind wants what is happening now, and the backlog endpoint is
/// there for anyone who needs to catch up.
#[utoipa::path(
    get, path = "/api/v1/logs/stream", tag = "logs",
    params(LogQuery),
    responses(
        (status = 200, description = "Log stream connection established", content_type = "text/event-stream")
    )
)]
pub async fn logs_sse(
    State(state): State<ApiState>,
    Query(query): Query<LogQuery>,
) -> Sse<impl futures_util::stream::Stream<Item = Result<SseEvent, std::convert::Infallible>>> {
    let rx = state.logs.subscribe();
    let level = query.level;
    let target = query.target;

    let stream =
        futures_util::stream::unfold((rx, level, target), |(mut rx, level, target)| async move {
            loop {
                match rx.recv().await {
                    Ok(line) => {
                        if !line.matches(level.as_deref(), target.as_deref()) {
                            continue;
                        }

                        let seq = line.seq;
                        let dto = LogLineDto::from(line);
                        let Ok(data) = serde_json::to_string(&dto) else {
                            // A line that cannot be serialised is skipped rather
                            // than ending the stream: one malformed field must not
                            // silence the log a developer is watching.
                            continue;
                        };

                        let event = SseEvent::default()
                            .event("log")
                            .id(seq.to_string())
                            .data(data);
                        return Some((Ok(event), (rx, level, target)));
                    }
                    // The reader fell behind; carry on from wherever the buffer is
                    // now rather than dropping the connection.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
                }
            }
        });

    Sse::new(stream).keep_alive(KeepAlive::default())
}

// --- Fault injection (Phase 8 A1, ADR-084) -----------------------------------

/// `GET /api/v1/faults` — the configured faults.
///
/// Always answerable: no fault configured is `active: false` with an empty
/// list, which is a true reading rather than a missing capability. That is why
/// this has no `503` the way the trace endpoints do (ADR-077).
#[utoipa::path(
    get, path = "/api/v1/faults", tag = "faults",
    responses(
        (status = 200, description = "The configured faults", body = FaultsDto),
    )
)]
pub async fn list_faults(State(state): State<ApiState>) -> Json<FaultsDto> {
    Json(snapshot_faults(&state))
}

/// `POST /api/v1/faults` — adds a fault rule.
///
/// Adds rather than replaces, so a scenario can build up several rules without
/// re-sending the ones already set. `DELETE` is the way to start over, which
/// keeps "add one" and "replace all" from being the same call with different
/// bodies.
#[utoipa::path(
    post, path = "/api/v1/faults", tag = "faults",
    request_body = FaultRuleDto,
    responses(
        (status = 201, description = "The fault was configured", body = FaultsDto),
        (status = 400, description = "Malformed request", body = crate::error::ProblemDetails),
    )
)]
pub async fn create_fault(
    State(state): State<ApiState>,
    Json(payload): Json<FaultRuleDto>,
) -> (StatusCode, Json<FaultsDto>) {
    let rule: sandesh_core::FaultRule = payload.into();
    // Logged at `warn` for the same reason the session logs a firing fault: a
    // server that starts behaving oddly should say when it was told to.
    tracing::warn!(mode = ?rule.mode, "fault rule configured");
    state.faults.add(rule);

    (StatusCode::CREATED, Json(snapshot_faults(&state)))
}

/// `DELETE /api/v1/faults` — clears every fault.
///
/// Returns the resulting (empty) state rather than `204`, so a caller that
/// clears and then asserts needs one request instead of two.
#[utoipa::path(
    delete, path = "/api/v1/faults", tag = "faults",
    responses(
        (status = 200, description = "Every fault was cleared", body = FaultsDto),
    )
)]
pub async fn clear_faults(State(state): State<ApiState>) -> Json<FaultsDto> {
    let cleared = state.faults.clear();
    if cleared > 0 {
        tracing::warn!(cleared, "fault rules cleared");
    }
    Json(snapshot_faults(&state))
}

/// The registry's current state as its DTO.
fn snapshot_faults(state: &ApiState) -> FaultsDto {
    let rules = state.faults.rules();
    FaultsDto {
        active: !rules.is_empty(),
        rules: rules.into_iter().map(Into::into).collect(),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn limit_defaults_and_is_capped() {
        assert_eq!(
            Pagination::default().limit().expect("default is valid"),
            DEFAULT_LIMIT
        );

        let capped = Pagination {
            limit: Some(MAX_LIMIT + 1),
            ..Pagination::default()
        };
        assert!(capped.limit().is_err(), "an absurd page size is rejected");

        let zero = Pagination {
            limit: Some(0),
            ..Pagination::default()
        };
        assert!(zero.limit().is_err(), "a zero page cannot make progress");

        let ok = Pagination {
            limit: Some(10),
            ..Pagination::default()
        };
        assert_eq!(ok.limit().expect("valid"), 10);
    }

    #[test]
    fn a_bad_identifier_is_a_client_error() {
        // Not a 500, and not a database lookup for something that can never match.
        let error = parse_id("../../etc/passwd", "message_id").expect_err("must reject");
        assert_eq!(error.status(), StatusCode::BAD_REQUEST);
        assert!(error.public_detail().contains("message_id"));
    }

    #[test]
    fn a_valid_uuid_parses() {
        assert!(parse_id("0192f3a1-0000-7000-8000-000000000000", "id").is_ok());
    }

    #[test]
    fn a_bad_cursor_is_rejected() {
        let page = Pagination {
            cursor: Some(String::from("'; DROP TABLE messages; --")),
            ..Pagination::default()
        };
        assert!(page.cursor_uid().is_err());
    }

    #[test]
    fn system_flags_and_keywords_are_separated() {
        let (system, keywords) =
            parse_flags(&[String::from("\\Seen"), String::from("important")]).expect("parses");
        assert!(system.contains(SystemFlags::SEEN));
        assert_eq!(keywords.len(), 1);
    }

    #[test]
    fn executable_content_types_are_never_echoed_back() {
        // An HTML or SVG part rendered inline in Sandesh's origin could act on
        // behalf of whoever is viewing it.
        for dangerous in [
            "text/html",
            "image/svg+xml",
            "application/javascript",
            "text/html; charset=utf-8",
        ] {
            assert!(
                !is_renderable_inline(dangerous),
                "{dangerous} must not be inline"
            );
        }
        assert!(is_renderable_inline("text/plain"));
        assert!(is_renderable_inline("image/png"));
    }

    #[test]
    fn renderable_attachment_types() {
        assert!(is_renderable_inline("application/pdf"));
        assert!(is_renderable_inline("image/jpeg"));
        assert!(is_renderable_inline("image/png"));
        assert!(is_renderable_inline("image/webp"));
        assert!(is_renderable_inline("image/gif"));
        assert!(!is_renderable_inline("application/zip"));
        assert!(!is_renderable_inline("text/html"));
    }
}

// --- Webhooks (Phase 8 A2, ADR-085, ADR-086) ---------------------------------

/// `GET /api/v1/webhooks` — the registered endpoints.
///
/// Always answerable, like `/faults`: no endpoint registered is an empty list,
/// which is a true reading rather than a missing capability.
#[utoipa::path(
    get, path = "/api/v1/webhooks", tag = "webhooks",
    responses(
        (status = 200, description = "The registered endpoints", body = [WebhookEndpointDto]),
    )
)]
/// # Errors
/// [`ApiError`] if the endpoints could not be read.
pub async fn list_webhooks(
    State(state): State<ApiState>,
) -> ApiResult<Json<Vec<WebhookEndpointDto>>> {
    let endpoints = state.outbox.endpoints().await?;
    Ok(Json(endpoints.into_iter().map(Into::into).collect()))
}

/// `POST /api/v1/webhooks` — registers an endpoint.
///
/// The URL is validated here rather than at send time. An endpoint that cannot
/// be parsed would otherwise be accepted, enqueue notifications for every
/// delivery, and fail all of them — turning a typo into a backlog of terminal
/// failures instead of a `400`.
#[utoipa::path(
    post, path = "/api/v1/webhooks", tag = "webhooks",
    request_body = NewWebhookDto,
    responses(
        (status = 201, description = "The endpoint was registered; the only response carrying `secret`", body = CreatedWebhookDto),
        (status = 400, description = "Malformed URL", body = crate::error::ProblemDetails),
    )
)]
/// # Errors
/// [`ApiError`] if the URL is not absolute, or the endpoint could not be stored.
pub async fn create_webhook(
    State(state): State<ApiState>,
    Json(payload): Json<NewWebhookDto>,
) -> ApiResult<(StatusCode, Json<CreatedWebhookDto>)> {
    if !payload.url.starts_with("http://") && !payload.url.starts_with("https://") {
        return Err(crate::error::ApiError::bad_request(
            "url must be an absolute http:// or https:// address",
        ));
    }

    let secret = if payload.sign {
        Some(match payload.secret {
            // A caller-supplied key is accepted so Sandesh can be pointed at a
            // receiver that already knows one, but an empty string is not a
            // key: it would sign every payload with a zero-length secret and
            // look like signing was on.
            Some(given) if !given.trim().is_empty() => given,
            Some(_) => {
                return Err(crate::error::ApiError::bad_request(
                    "secret must not be empty when sign is set",
                ));
            }
            None => generate_webhook_secret()?,
        })
    } else {
        None
    };

    let endpoint = sandesh_core::WebhookEndpoint {
        id: sandesh_core::EndpointId::new(),
        url: payload.url,
        enabled: payload.enabled,
        created_at: sandesh_core::Timestamp::now(),
        secret: secret.clone().map(sandesh_core::WebhookSecret::new),
    };
    state.outbox.register_endpoint(&endpoint).await?;

    // The only response that carries the secret. Everything that reads an
    // endpoint afterwards sees `has_secret` and nothing more.
    Ok((
        StatusCode::CREATED,
        Json(CreatedWebhookDto {
            endpoint: endpoint.into(),
            secret,
        }),
    ))
}

/// Mints a webhook signing secret.
///
/// 32 bytes from the OS CSPRNG, hex-encoded, with a `whsec_` prefix so a key
/// found in a config file or a log is identifiable as one — the same reason
/// Stripe and GitHub prefix theirs. 256 bits is the block size of the SHA-256
/// HMAC it keys, so a longer secret would buy nothing.
///
/// # Errors
/// [`ApiError::Internal`] if the OS random number generator is unavailable.
/// Failing the request is the only safe response: returning a key from a
/// fallback source would produce a signature that looks real and is not.
fn generate_webhook_secret() -> ApiResult<String> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|error| {
        ApiError::Internal(format!(
            "could not generate a signing secret: the OS random number generator failed ({error})"
        ))
    })?;
    Ok(format!("whsec_{}", hex::encode(bytes)))
}

/// `DELETE /api/v1/webhooks/{id}` — removes an endpoint and everything queued
/// for it.
///
/// The cascade is deliberate: a pending notification for a destination that no
/// longer exists is undeliverable, and letting it retry its way to terminal
/// failure would fill the outbox view with noise about a decision already made.
#[utoipa::path(
    delete, path = "/api/v1/webhooks/{id}", tag = "webhooks",
    params(("id" = String, Path, description = "Endpoint identifier")),
    responses(
        (status = 204, description = "The endpoint was removed"),
        (status = 404, description = "No such endpoint", body = crate::error::ProblemDetails),
    )
)]
/// # Errors
/// [`ApiError`] if the id is malformed or names no registered endpoint.
pub async fn delete_webhook(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    let id = sandesh_core::EndpointId::from_uuid(
        id.parse()
            .map_err(|_| crate::error::ApiError::bad_request("malformed endpoint id"))?,
    );

    if state.outbox.remove_endpoint(id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(crate::error::ApiError::from(
            sandesh_core::CoreError::NotFound {
                entity: sandesh_core::EntityKind::WebhookEndpoint,
                id: id.to_string(),
            },
        ))
    }
}

/// Paging for the outbox listing.
#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct OutboxQuery {
    /// Page size. Defaults and caps as elsewhere.
    pub limit: Option<u32>,
}

/// `GET /api/v1/webhooks/outbox` — recent notifications and their outcomes.
///
/// Includes delivered and terminally-failed entries, not only what is
/// outstanding: the question this view answers is "did my webhook fire?", and
/// a list of only pending work cannot answer it.
#[utoipa::path(
    get, path = "/api/v1/webhooks/outbox", tag = "webhooks",
    params(OutboxQuery),
    responses(
        (status = 200, description = "Recent outbox entries", body = [OutboxEntryDto]),
    )
)]
/// # Errors
/// [`ApiError`] if the outbox could not be read.
pub async fn list_outbox(
    State(state): State<ApiState>,
    Query(params): Query<OutboxQuery>,
) -> ApiResult<Json<Vec<OutboxEntryDto>>> {
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT);
    let entries = state.outbox.recent(limit).await?;
    Ok(Json(entries.into_iter().map(Into::into).collect()))
}

// --- Message rules (Phase 8 A3, ADR-087) -------------------------------------

/// `GET /api/v1/rules` — the configured rules, in evaluation order.
#[utoipa::path(
    get, path = "/api/v1/rules", tag = "rules",
    responses(
        (status = 200, description = "The configured rules", body = [RuleDto]),
    )
)]
/// # Errors
/// [`ApiError`] if the rules could not be read.
pub async fn list_rules(State(state): State<ApiState>) -> ApiResult<Json<Vec<RuleDto>>> {
    let rules = state.rules.all().await?;
    Ok(Json(rules.into_iter().map(Into::into).collect()))
}

/// `POST /api/v1/rules` — creates a rule.
///
/// The keyword is validated here, not at delivery time. An invalid one would
/// otherwise be stored, fail to decode on every read, and silently never fire —
/// a rule that exists in the list and does nothing, which is worse than a `400`.
///
/// An empty condition list is refused for the same reason it matches nothing in
/// the domain: a rule that tags every message is never what a half-finished
/// configuration meant, and refusing it here is the only place it can be said
/// out loud.
#[utoipa::path(
    post, path = "/api/v1/rules", tag = "rules",
    request_body = NewRuleDto,
    responses(
        (status = 201, description = "The rule was created", body = RuleDto),
        (status = 400, description = "Invalid keyword or no conditions", body = crate::error::ProblemDetails),
    )
)]
/// # Errors
/// [`ApiError`] if the keyword is invalid, no conditions were given, or the
/// rule could not be stored.
pub async fn create_rule(
    State(state): State<ApiState>,
    Json(payload): Json<NewRuleDto>,
) -> ApiResult<(StatusCode, Json<RuleDto>)> {
    let Some(keyword) = sandesh_core::Keyword::new(payload.add_keyword) else {
        return Err(crate::error::ApiError::bad_request(
            "add_keyword must be a valid IMAP keyword: no leading backslash, \
             no spaces or atom-specials",
        ));
    };

    if payload.conditions.is_empty() {
        return Err(crate::error::ApiError::bad_request(
            "a rule needs at least one condition; one with none would match nothing",
        ));
    }

    let rule = sandesh_core::Rule {
        id: sandesh_core::RuleId::new(),
        name: payload.name,
        conditions: payload.conditions.into_iter().map(Into::into).collect(),
        action: sandesh_core::RuleAction::AddKeyword(keyword),
        enabled: payload.enabled,
        created_at: sandesh_core::Timestamp::now(),
    };
    state.rules.save(&rule).await?;

    Ok((StatusCode::CREATED, Json(rule.into())))
}

/// `DELETE /api/v1/rules/{id}` — removes a rule.
///
/// Deliveries that already recorded it keep their record: the match stored the
/// rule's name rather than a reference, so removing a rule cannot erase the
/// explanation of what it did while it existed (ADR-087).
#[utoipa::path(
    delete, path = "/api/v1/rules/{id}", tag = "rules",
    params(("id" = String, Path, description = "Rule identifier")),
    responses(
        (status = 204, description = "The rule was removed"),
        (status = 404, description = "No such rule", body = crate::error::ProblemDetails),
    )
)]
/// # Errors
/// [`ApiError`] if the id is malformed or names no configured rule.
pub async fn delete_rule(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    let id = sandesh_core::RuleId::from_uuid(
        id.parse()
            .map_err(|_| crate::error::ApiError::bad_request("malformed rule id"))?,
    );

    if state.rules.remove(id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(crate::error::ApiError::from(
            sandesh_core::CoreError::NotFound {
                entity: sandesh_core::EntityKind::Rule,
                id: id.to_string(),
            },
        ))
    }
}

// ---------------------------------------------------------------------------
// Replay and bulk import/export (Phase 8 A5)
// ---------------------------------------------------------------------------

/// The header carrying a replayed message's provenance.
///
/// Written into the replayed bytes so the link travels with the message —
/// through export, IMAP `FETCH` and `/raw` alike. The queryable half of the
/// same fact is `deliveries.replay_of_message_id`.
pub const REPLAY_OF_HEADER: &str = "X-Sandesh-Replay-Of";

/// Rewrites a message's `Message-ID` and stamps its replay provenance.
///
/// A replay is a *new* message, not a second copy of the old one: reusing the
/// `Message-ID` would make the two indistinguishable to every downstream
/// consumer that deduplicates on it, which is most of them. The original id is
/// preserved in the `X-Sandesh-Replay-Of` header rather than discarded.
///
/// Both headers are inserted at the top of the block, before any existing
/// header, so the rewrite never has to find the end of a folded value. The
/// body is untouched: only the header block is rewritten, and it is separated
/// from the body by the first blank line.
fn stamp_replay_headers(raw: &[u8], original_message_id: Option<&str>, fresh_id: &str) -> Vec<u8> {
    // Detect the line ending the message actually uses, so the inserted
    // headers match it. A bare-LF message must not gain CRLF headers.
    let eol: &[u8] = if find_subslice(raw, b"\r\n")
        .is_some_and(|crlf| find_subslice(raw, b"\n").is_none_or(|lf| crlf <= lf))
    {
        b"\r\n"
    } else {
        b"\n"
    };

    let mut out = Vec::with_capacity(raw.len() + 256);

    out.extend_from_slice(b"Message-ID: ");
    out.extend_from_slice(fresh_id.as_bytes());
    out.extend_from_slice(eol);

    if let Some(original) = original_message_id {
        out.extend_from_slice(REPLAY_OF_HEADER.as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(original.as_bytes());
        out.extend_from_slice(eol);
    }

    // Copy the original, dropping any Message-ID it already carried — leaving
    // it would produce a message with two, which RFC 5322 forbids and parsers
    // resolve inconsistently.
    out.extend_from_slice(&strip_header(raw, b"Message-ID", eol));

    out
}

/// Removes every occurrence of one header from a message's header block.
///
/// Continuation lines (leading space or tab) belong to the header above them
/// and are removed with it. Stops at the blank line that ends the block, so a
/// body line that happens to look like the header is untouched.
fn strip_header(raw: &[u8], name: &[u8], eol: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    let mut rest = raw;
    let mut dropping = false;

    loop {
        let (line_with_term, next) = match rest.iter().position(|&b| b == b'\n') {
            Some(nl) => (&rest[..=nl], &rest[nl + 1..]),
            None => (rest, &rest[rest.len()..]),
        };

        let line = {
            let no_lf = line_with_term.strip_suffix(b"\n").unwrap_or(line_with_term);
            no_lf.strip_suffix(b"\r").unwrap_or(no_lf)
        };

        if line.is_empty() {
            // End of the header block: copy the rest verbatim.
            out.extend_from_slice(rest);
            return out;
        }

        let is_continuation = line.first().is_some_and(|b| *b == b' ' || *b == b'\t');

        if is_continuation {
            if !dropping {
                out.extend_from_slice(line_with_term);
            }
        } else {
            dropping = line.len() > name.len()
                && line[..name.len()].eq_ignore_ascii_case(name)
                && line[name.len()] == b':';
            if !dropping {
                out.extend_from_slice(line_with_term);
            }
        }

        if next.is_empty() {
            // A header block with no body and no terminating blank line.
            if !out.ends_with(eol) {
                out.extend_from_slice(eol);
            }
            return out;
        }
        rest = next;
    }
}

/// The first index at which `needle` occurs in `haystack`.
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// `POST /api/v1/messages/{id}/replay`.
///
/// Re-delivers a stored message through the same placement path a fresh
/// injection uses, with a new `Message-ID` and a recorded link back to the
/// source (`X-Sandesh-Replay-Of` in the bytes, `replay_of` on the delivery).
///
/// # Errors
/// `400` for an unparseable id, `404` if no such message exists.
#[utoipa::path(
    post, path = "/api/v1/messages/{id}/replay", tag = "messages",
    params(("id" = String, Path, description = "Message id (UUID) to replay")),
    responses(
        (status = 201, description = "The message was replayed as a new message", body = MessageSummary),
        (status = 400, description = "Malformed request", body = crate::error::ProblemDetails),
        (status = 404, description = "No such message", body = crate::error::ProblemDetails),
    )
)]
pub async fn replay_message(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> ApiResult<(StatusCode, Json<MessageSummary>)> {
    let message_id = MessageId::from_uuid(parse_id(&id, "message_id")?);

    let raw = state
        .messages
        .raw(message_id)
        .await?
        .ok_or(ApiError::NotFound("Message"))?;

    let parsed = sandesh_core::parse(raw.bytes());
    // The parser strips the angle brackets from a `Message-ID`, but a msg-id is
    // only well-formed inside them (RFC 5322 §3.6.4). Restore them, so the
    // header we write is one a reader can match against the original.
    let original_message_id = parsed.header("Message-ID").map(|h| {
        let value = h.decoded_value.trim();
        if value.starts_with('<') && value.ends_with('>') {
            value.to_owned()
        } else {
            format!("<{value}>")
        }
    });

    let fresh_id = format!("<{}@sandesh.replay>", uuid::Uuid::now_v7());
    let stamped = stamp_replay_headers(raw.bytes(), original_message_id.as_deref(), &fresh_id);

    let inserted = place_raw_message(&state, stamped, Some(message_id), None).await?;

    let message = state
        .messages
        .get(inserted.message_id)
        .await?
        .ok_or(ApiError::NotFound("Message"))?;
    let names = mailbox_names(&state, &inserted.entries).await?;
    let summary = MessageSummary::from_core(&message, entry_refs(&inserted.entries, &names));

    Ok((StatusCode::CREATED, Json(summary)))
}

/// Query parameters for `GET /api/v1/export`.
#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ExportParams {
    /// `mbox` (default) or `eml`.
    ///
    /// `eml` exports a single message and requires exactly one to be selected;
    /// `mbox` frames any number, including none.
    #[serde(default)]
    pub format: Option<String>,
    /// Restrict the export to one account.
    #[serde(default)]
    pub account_id: Option<String>,
    /// Restrict the export to one mailbox.
    #[serde(default)]
    pub mailbox_id: Option<String>,
    /// Maximum messages to export.
    #[serde(default)]
    pub limit: Option<u32>,
}

/// The default and maximum number of messages one export may carry.
const EXPORT_DEFAULT_LIMIT: u32 = 1_000;
const EXPORT_MAX_LIMIT: u32 = 10_000;

/// `GET /api/v1/export`.
///
/// Bulk export in `mboxrd` framing (or a single `.eml`). The framing is
/// `mboxrd` specifically so that the export re-imports byte-identically —
/// see `sandesh_core::mbox` for why `mboxo` cannot.
///
/// # Errors
/// `400` for an unknown format or an unparseable id.
#[utoipa::path(
    get, path = "/api/v1/export", tag = "messages",
    params(ExportParams),
    responses(
        (status = 200, description = "An mbox (or a single .eml) of the selected messages"),
        (status = 400, description = "Malformed request", body = crate::error::ProblemDetails),
    )
)]
pub async fn export_messages(
    State(state): State<ApiState>,
    Query(params): Query<ExportParams>,
) -> ApiResult<Response> {
    let format = params.format.as_deref().unwrap_or("mbox");
    if !matches!(format, "mbox" | "eml") {
        return Err(ApiError::bad_request("format must be 'mbox' or 'eml'"));
    }

    let limit = params
        .limit
        .unwrap_or(EXPORT_DEFAULT_LIMIT)
        .min(EXPORT_MAX_LIMIT);

    // Reuse the listing's scope resolution so export selects exactly what the
    // message list shows, rather than a second notion of "which messages".
    let scope = Pagination {
        account_id: params.account_id.clone(),
        mailbox_id: params.mailbox_id.clone(),
        ..Pagination::default()
    };
    let mailboxes = scope_mailboxes(&state, &scope).await?;

    // Deduplicate by message: a message placed in several in-scope mailboxes is
    // one message and must be exported once, or the round trip would multiply
    // it. `BTreeMap` keyed by the newest placement time gives newest-first
    // order, matching the listing.
    let mut newest: BTreeMap<MessageId, Timestamp> = BTreeMap::new();
    for mailbox_id in mailboxes {
        for entry in state.messages.list_entries(mailbox_id).await? {
            newest
                .entry(entry.message_id)
                .and_modify(|at| {
                    if entry.added_at > *at {
                        *at = entry.added_at;
                    }
                })
                .or_insert(entry.added_at);
        }
    }

    let mut ordered: Vec<(MessageId, Timestamp)> = newest.into_iter().collect();
    ordered.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    ordered.truncate(limit as usize);

    let mut messages = Vec::with_capacity(ordered.len());
    for (message_id, _) in ordered {
        if let Some(message) = state.messages.get(message_id).await? {
            messages.push(message);
        }
    }

    if format == "eml" {
        if messages.len() != 1 {
            return Err(ApiError::bad_request(
                "format 'eml' exports exactly one message; narrow the selection or use 'mbox'",
            ));
        }
        let raw = state
            .messages
            .raw(messages[0].id)
            .await?
            .ok_or(ApiError::NotFound("Message"))?;
        let bytes = raw.bytes().to_vec();
        return Ok((
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "message/rfc822"),
                (header::CONTENT_LENGTH, &bytes.len().to_string()),
                (
                    header::CONTENT_DISPOSITION,
                    "attachment; filename=\"message.eml\"",
                ),
            ],
            Body::from(bytes),
        )
            .into_response());
    }

    let mut out = Vec::new();
    for message in &messages {
        if let Some(raw) = state.messages.raw(message.id).await? {
            out.extend_from_slice(&sandesh_core::write_mbox_entry(
                raw.bytes(),
                message.received_at,
            ));
        }
    }

    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/mbox"),
            (header::CONTENT_LENGTH, &out.len().to_string()),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"sandesh-export.mbox\"",
            ),
        ],
        Body::from(out),
    )
        .into_response())
}

/// Query parameters for `POST /api/v1/import`.
#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ImportParams {
    /// `mbox` (default) or `eml`.
    #[serde(default)]
    pub format: Option<String>,
    /// Optional target mailbox ID to place all imported messages directly into.
    #[serde(default)]
    pub mailbox_id: Option<String>,
}

/// What an import did.
#[derive(Debug, serde::Serialize, ToSchema)]
pub struct ImportResponse {
    /// How many messages the payload was framed into.
    pub parsed: usize,
    /// How many were stored and placed.
    pub imported: usize,
    /// The ids of the stored messages, in payload order.
    pub message_ids: Vec<String>,
}

/// `POST /api/v1/import`.
///
/// Bulk import of an `mboxrd` file or a single `.eml`. Each framed message goes
/// through the same placement path as an injection, so an imported message is
/// indistinguishable from one that arrived over SMTP.
///
/// # Errors
/// `400` for an unknown format, an empty body, or a payload that frames into
/// no messages.
#[utoipa::path(
    post, path = "/api/v1/import", tag = "messages",
    params(ImportParams),
    request_body(content = String, description = "An mbox file or a single RFC 822 message", content_type = "application/mbox"),
    responses(
        (status = 200, description = "What was imported", body = ImportResponse),
        (status = 400, description = "Malformed request", body = crate::error::ProblemDetails),
    )
)]
pub async fn import_messages(
    State(state): State<ApiState>,
    Query(params): Query<ImportParams>,
    body: Bytes,
) -> ApiResult<(StatusCode, Json<ImportResponse>)> {
    let format = params.format.as_deref().unwrap_or("mbox");
    if !matches!(format, "mbox" | "eml") {
        return Err(ApiError::bad_request("format must be 'mbox' or 'eml'"));
    }

    if body.is_empty() {
        return Err(ApiError::bad_request("import body cannot be empty"));
    }

    let messages: Vec<Vec<u8>> = if format == "eml" {
        vec![body.to_vec()]
    } else {
        sandesh_core::parse_mbox(&body)
    };

    if messages.is_empty() {
        return Err(ApiError::bad_request(
            "no messages found; an mbox must begin with a 'From ' separator line",
        ));
    }

    let target_mailbox_id = match params.mailbox_id.as_deref() {
        Some(s) if !s.trim().is_empty() => {
            let id = MailboxId::from_uuid(parse_id(s.trim(), "mailbox_id")?);
            Some(id)
        }
        _ => None,
    };

    let parsed = messages.len();
    let mut message_ids = Vec::with_capacity(parsed);

    for raw in messages {
        // A zero-byte message is imported, not skipped. `testdata/malformed/
        // empty.eml` is a deliberate fixture, and Phase 2's rule is that
        // malformed mail is *stored*, not rejected — skipping it here would
        // make the corpus round trip quietly lose one message, which is
        // exactly the silent failure this increment's test exists to catch.
        let inserted = place_raw_message(&state, raw, None, target_mailbox_id).await?;
        message_ids.push(inserted.message_id.to_string());
    }

    Ok((
        StatusCode::OK,
        Json(ImportResponse {
            imported: message_ids.len(),
            parsed,
            message_ids,
        }),
    ))
}

// ---------------------------------------------------------------------------
// Workspaces
// ---------------------------------------------------------------------------

/// `GET /api/v1/workspaces` — lists all workspaces.
///
/// # Errors
/// Propagates a storage failure as `500`.
#[utoipa::path(
    get,
    path = "/api/v1/workspaces",
    tag = "workspaces",
    responses(
        (status = 200, description = "List of all workspaces", body = Vec<WorkspaceDto>),
    )
)]
pub async fn list_workspaces(State(state): State<ApiState>) -> ApiResult<Json<Vec<WorkspaceDto>>> {
    let list = state.workspaces.list().await?;
    Ok(Json(list.iter().map(WorkspaceDto::from_core).collect()))
}

/// `POST /api/v1/workspaces` — creates a new workspace.
///
/// # Errors
/// Returns `400` for an invalid or already-used identifier, `500` on storage failure.
#[utoipa::path(
    post,
    path = "/api/v1/workspaces",
    tag = "workspaces",
    request_body = CreateWorkspaceRequest,
    responses(
        (status = 201, description = "Workspace created", body = WorkspaceDto),
        (status = 400, description = "Invalid request", body = crate::error::ProblemDetails),
    )
)]
pub async fn create_workspace(
    State(state): State<ApiState>,
    Json(req): Json<CreateWorkspaceRequest>,
) -> ApiResult<(StatusCode, Json<WorkspaceDto>)> {
    let name = req.name.trim();
    if name.is_empty() {
        return Err(ApiError::bad_request("workspace name cannot be empty"));
    }

    let slug = match req.id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(custom) => custom.to_lowercase(),
        None => slugify(name),
    };

    let password_hash = match req
        .password
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(pass) => Some(hash_account_password(&state, pass)?),
        None => None,
    };

    let initial_account = req
        .initial_account
        .map(|ia| sandesh_core::InitialAccountSetup {
            username: ia.username,
            addresses: ia.addresses,
            is_catch_all: ia.is_catch_all,
            password_hash: password_hash.clone(),
        })
        .or_else(|| {
            password_hash.map(|ph| sandesh_core::InitialAccountSetup {
                username: "dev".to_string(),
                addresses: Vec::new(),
                is_catch_all: true,
                password_hash: Some(ph),
            })
        });

    let ws = state
        .workspaces
        .create(
            &slug,
            name,
            req.description.as_deref(),
            req.clone_from.as_deref(),
            initial_account,
            req.auth_required,
        )
        .await?;

    if req.set_active {
        state.workspaces.set_active(&slug).await?;
    }

    let refreshed = state.workspaces.get(&slug).await?.unwrap_or(ws);
    Ok((
        StatusCode::CREATED,
        Json(WorkspaceDto::from_core(&refreshed)),
    ))
}

/// `PATCH /api/v1/workspaces/{id}` — updates a workspace (name, description, `auth_required`).
#[utoipa::path(
    patch,
    path = "/api/v1/workspaces/{id}",
    tag = "workspaces",
    params(
        ("id" = String, Path, description = "Workspace slug identifier")
    ),
    request_body = UpdateWorkspaceRequest,
    responses(
        (status = 200, description = "Workspace updated", body = WorkspaceDto),
        (status = 404, description = "No such workspace", body = crate::error::ProblemDetails),
    )
)]
///
/// # Errors
///
/// Returns [`ApiError`] if no workspace has this identifier, or if the update
/// cannot be persisted.
pub async fn update_workspace(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(req): Json<UpdateWorkspaceRequest>,
) -> ApiResult<Json<WorkspaceDto>> {
    let update = sandesh_core::WorkspaceUpdate {
        name: req.name,
        description: req.description,
        auth_required: req.auth_required,
    };
    let ws = state.workspaces.update(&id, update).await?;
    Ok(Json(WorkspaceDto::from_core(&ws)))
}

/// `GET /api/v1/workspaces/{id}` — fetches a workspace.
///
/// # Errors
/// Returns `404` when no workspace has that identifier, `500` on storage failure.
#[utoipa::path(
    get,
    path = "/api/v1/workspaces/{id}",
    tag = "workspaces",
    params(
        ("id" = String, Path, description = "Workspace slug identifier")
    ),
    responses(
        (status = 200, description = "Workspace details", body = WorkspaceDto),
        (status = 404, description = "No such workspace", body = crate::error::ProblemDetails),
    )
)]
pub async fn get_workspace(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> ApiResult<Json<WorkspaceDto>> {
    let ws = state
        .workspaces
        .get(&id)
        .await?
        .ok_or(ApiError::NotFound("workspace"))?;
    Ok(Json(WorkspaceDto::from_core(&ws)))
}

/// `POST /api/v1/workspaces/{id}/activate` — sets the active workspace.
///
/// # Errors
/// Returns `404` when no workspace has that identifier, `500` on storage failure.
#[utoipa::path(
    post,
    path = "/api/v1/workspaces/{id}/activate",
    tag = "workspaces",
    params(
        ("id" = String, Path, description = "Workspace slug identifier")
    ),
    responses(
        (status = 200, description = "Workspace activated", body = WorkspaceDto),
        (status = 404, description = "No such workspace", body = crate::error::ProblemDetails),
    )
)]
pub async fn activate_workspace(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> ApiResult<Json<WorkspaceDto>> {
    state.workspaces.set_active(&id).await?;
    let ws = state
        .workspaces
        .get(&id)
        .await?
        .ok_or(ApiError::NotFound("workspace"))?;
    Ok(Json(WorkspaceDto::from_core(&ws)))
}

/// `POST /api/v1/workspaces/{id}/purge` — purges mail and traces in a workspace.
///
/// # Errors
/// Returns `404` when no workspace has that identifier, `500` on storage failure.
#[utoipa::path(
    post,
    path = "/api/v1/workspaces/{id}/purge",
    tag = "workspaces",
    params(
        ("id" = String, Path, description = "Workspace slug identifier")
    ),
    request_body(content = Option<PurgeWorkspaceRequest>),
    responses(
        (status = 200, description = "Workspace purged", body = PurgeResultDto),
        (status = 404, description = "No such workspace", body = crate::error::ProblemDetails),
    )
)]
pub async fn purge_workspace(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    payload: Option<Json<PurgeWorkspaceRequest>>,
) -> ApiResult<Json<PurgeResultDto>> {
    let options = parse_purge_options(payload.map(|Json(p)| p));
    let res = state.workspaces.purge(&id, options).await?;
    Ok(Json(PurgeResultDto::from_core(&res)))
}

/// `POST /api/v1/purge` — purges the currently active workspace.
///
/// # Errors
/// Propagates a storage failure as `500`.
#[utoipa::path(
    post,
    path = "/api/v1/purge",
    tag = "workspaces",
    request_body(content = Option<PurgeWorkspaceRequest>),
    responses(
        (status = 200, description = "Active workspace purged", body = PurgeResultDto),
    )
)]
pub async fn purge_active_workspace(
    State(state): State<ApiState>,
    payload: Option<Json<PurgeWorkspaceRequest>>,
) -> ApiResult<Json<PurgeResultDto>> {
    // Use active_id() directly: a single registry read rather than a full
    // list() that computes disk-size stats for every workspace.
    let active_id = state.workspaces.active_id().await?;

    let options = parse_purge_options(payload.map(|Json(p)| p));
    let res = state.workspaces.purge(&active_id, options).await?;
    Ok(Json(PurgeResultDto::from_core(&res)))
}

/// `DELETE /api/v1/workspaces/{id}` — deletes a workspace and removes its database file.
///
/// # Errors
/// Returns `400` for the default workspace, `404` when no workspace has that
/// identifier, `500` on storage failure.
#[utoipa::path(
    delete,
    path = "/api/v1/workspaces/{id}",
    tag = "workspaces",
    params(
        ("id" = String, Path, description = "Workspace slug identifier")
    ),
    responses(
        (status = 204, description = "Workspace deleted"),
        (status = 400, description = "Cannot delete default workspace", body = crate::error::ProblemDetails),
        (status = 404, description = "No such workspace", body = crate::error::ProblemDetails),
    )
)]
pub async fn delete_workspace(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    if id == "default" {
        return Err(ApiError::bad_request("cannot delete the default workspace"));
    }
    let deleted = state.workspaces.delete(&id).await?;
    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound("workspace"))
    }
}

/// `GET /api/v1/workspaces/{id}/export` — downloads the workspace `SQLite` database file.
///
/// # Errors
/// Returns `404` when no workspace has that identifier, `500` if the database
/// file cannot be read.
pub async fn export_workspace_db(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    let ws = state
        .workspaces
        .get(&id)
        .await?
        .ok_or(ApiError::NotFound("workspace"))?;

    // Checkpoint any outstanding WAL frames into the database file so the
    // exported file includes all committed writes.
    let _ = state.workspaces.checkpoint(&id).await;

    let path = std::path::Path::new(&ws.database_path);
    if !path.exists() {
        return Err(ApiError::NotFound("database file"));
    }

    let bytes = tokio::fs::read(path)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to read database: {e}")))?;

    let filename = format!("{}.db", ws.id);
    let disposition = format!("attachment; filename=\"{filename}\"");

    let response = Response::builder()
        .header(header::CONTENT_TYPE, "application/x-sqlite3")
        .header(header::CONTENT_DISPOSITION, disposition)
        .body(Body::from(bytes))
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    Ok(response)
}

fn parse_purge_options(req: Option<PurgeWorkspaceRequest>) -> sandesh_core::PurgeOptions {
    match req {
        Some(r) => {
            let mode = if r.mode.eq_ignore_ascii_case("factory_reset") {
                sandesh_core::PurgeMode::FactoryReset
            } else {
                sandesh_core::PurgeMode::MailAndTraces
            };
            sandesh_core::PurgeOptions {
                mode,
                clear_traces: r.clear_traces,
                clear_webhooks: r.clear_webhooks,
            }
        }
        None => sandesh_core::PurgeOptions::default(),
    }
}

fn slugify(name: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = true;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    let trimmed = slug.trim_end_matches('-').to_string();
    if trimmed.is_empty() {
        "workspace".to_string()
    } else {
        trimmed
    }
}
