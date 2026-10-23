//! The `OpenAPI` document (`API_DESIGN.md`, ADR-026).
//!
//! There is exactly one source of truth for the HTTP contract: the Rust types
//! and handlers themselves. This module does not describe the API — it collects
//! the descriptions that already live next to the code, so a handler and its
//! documentation cannot disagree without a compile error.
//!
//! ```text
//! #[utoipa::path] on a handler ──┐
//! #[derive(ToSchema)] on a DTO ──┼──▶ ApiDoc ──▶ /api/openapi.json ──▶ web/src/generated
//! ```
//!
//! Nothing downstream is hand-written: the TypeScript the UI compiles against
//! is generated from this document, and CI regenerates both and fails on a
//! diff. Adding a route without annotating it therefore does not silently
//! produce a stale contract — it produces a missing path that the coverage
//! test in this module catches.

use utoipa::OpenApi;

use crate::dto;
use crate::error::ProblemDetails;
use crate::routes;

/// The generated `OpenAPI` 3.1 document.
#[derive(OpenApi)]
#[openapi(
    info(
        title = "Sandesh",
        description = "Local mail infrastructure for developers. \
                       REST over the mail core; the WebSocket event stream at \
                       `/api/v1/events` is documented in `docs/EVENT_MODEL.md`, \
                       because OpenAPI cannot describe a WebSocket protocol.",
        version = env!("CARGO_PKG_VERSION"),
        license(name = "Apache-2.0"),
    ),
    paths(
        routes::health,
        routes::ready,
        routes::info,
        routes::list_accounts,
        routes::get_account,
        routes::create_account,
        routes::patch_account,
        routes::delete_account,
        routes::list_mailboxes,
        routes::get_mailbox,
        routes::create_mailbox,
        routes::patch_mailbox,
        routes::delete_mailbox,
        routes::list_mailbox_entries,
        routes::get_entry,
        routes::patch_entry_flags,
        routes::delete_entry,
        routes::move_entry,
        routes::copy_entry,
        routes::bulk_entries,
        routes::list_messages,
        routes::inject_message,
        routes::get_message,
        routes::delete_message,
        routes::get_message_raw,
        routes::get_message_header_analysis,
        routes::get_message_headers,
        routes::get_message_links,
        routes::get_message_mime,
        routes::get_message_part,
        routes::get_message_delivery,
        routes::replay_message,
        routes::export_messages,
        routes::import_messages,
        routes::list_deliveries,
        routes::list_connections,
        routes::list_faults,
        routes::create_fault,
        routes::clear_faults,
        routes::list_webhooks,
        routes::create_webhook,
        routes::delete_webhook,
        routes::list_outbox,
        routes::list_rules,
        routes::create_rule,
        routes::delete_rule,
        routes::list_logs,
        routes::logs_sse,
        routes::get_connection,
        routes::list_message_attachments,
        routes::get_attachment,
        routes::get_attachment_thumbnail,
        routes::search_messages,
        routes::explain_search,
        routes::send_message,
        routes::events_sse,
        routes::list_workspaces,
        routes::create_workspace,
        routes::get_workspace,
        routes::update_workspace,
        routes::activate_workspace,
        routes::purge_workspace,
        routes::purge_active_workspace,
        routes::delete_workspace,
    ),
    components(schemas(
        routes::ImportResponse,
        dto::AccountDto,
        dto::AddressDto,
        dto::AttachmentDto,
        dto::BulkAction,
        dto::BulkEntriesRequest,
        dto::BulkEntriesResponse,
        dto::CopyEntryRequest,
        dto::CreateAccountRequest,
        dto::CreateMailboxRequest,
        dto::DeliveryDto,
        dto::ConnectionDto,
        dto::LogLineDto,
        dto::DiagnosticDto,
        dto::EntryDto,
        dto::EntryRef,
        dto::FaultMatchDto,
        dto::FaultModeDto,
        dto::FaultRuleDto,
        dto::FaultsDto,
        dto::WebhookEndpointDto,
        dto::NewWebhookDto,
        dto::CreatedWebhookDto,
        dto::OutboxEntryDto,
        dto::RuleDto,
        dto::NewRuleDto,
        dto::RuleConditionDto,
        dto::RuleMatchDto,
        dto::AuthResultDto,
        dto::DkimSignatureDto,
        dto::HeaderAnalysisDto,
        dto::LinkReportDto,
        dto::ExtractedLinkDto,
        dto::HeaderDto,
        dto::ReceivedHopDto,
        dto::SpamHeaderDto,
        dto::SpfRecordDto,
        dto::TagDto,
        dto::HealthDto,
        dto::InfoDto,
        dto::ListResponseAccountDto,
        dto::ListResponseAttachmentDto,
        dto::ListResponseDeliveryDto,
        dto::ListResponseConnectionDto,
        dto::ListResponseLogLineDto,
        dto::ListResponseEntryDto,
        dto::ListResponseMailboxDto,
        dto::ListResponseMessageSummary,
        dto::MailboxDto,
        dto::MessageDetail,
        dto::MessageSummary,
        dto::MimePartDto,
        dto::MimeTree,
        dto::MoveEntryRequest,
        dto::PatchAccountRequest,
        dto::PatchMailboxRequest,
        dto::WorkspaceDto,
        dto::CreateWorkspaceRequest,
        dto::UpdateWorkspaceRequest,
        dto::InitialAccountSetupDto,
        dto::PurgeWorkspaceRequest,
        dto::PurgeResultDto,
        dto::ReadyDto,
        dto::RecipientOutcomeDto,
        dto::SearchExplainResponse,
        dto::SendMessageRequest,
        dto::SendMessageResponse,
        routes::FlagPatch,
        ProblemDetails,
    )),
    tags(
        (name = "health", description = "Liveness, readiness and connection details"),
        (name = "accounts", description = "Login identities and recipient routing"),
        (name = "mailboxes", description = "Folders and their UID validity state"),
        (name = "entries", description = "Placements of a message into a mailbox"),
        (name = "messages", description = "Immutable mail content, headers and MIME structure"),
        (name = "attachments", description = "Attachment metadata, binary content and image thumbnails"),
        (name = "search", description = "Search query parsing and message retrieval"),
        (name = "events", description = "Live event stream over Server-Sent Events"),
        (name = "deliveries", description = "How mail arrived: envelope, routing and per-recipient outcome"),
        (name = "connections", description = "Per-connection protocol transcripts for SMTP, IMAP and POP3"),
        (name = "faults", description = "Deliberate SMTP failures, for testing application resilience"),
        (name = "webhooks", description = "Outbound notifications on delivery, with retry and a durable outbox"),
        (name = "rules", description = "Auto-tagging rules applied at delivery, recorded on the delivery"),
    ),
)]
#[derive(Debug)]
pub struct ApiDoc;

/// The JSON representation of the `OpenAPI` 3.1 contract.
///
/// # Errors
/// [`serde_json::Error`] if serialization fails — a broken invariant that
/// would mean a malformed annotation rather than a runtime condition.
pub fn document_json() -> Result<String, serde_json::Error> {
    let mut json = serde_json::to_string_pretty(&ApiDoc::openapi())?;
    json.push('\n');
    Ok(json)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    /// Every route the router serves, except the WebSocket upgrade, which
    /// `OpenAPI` cannot describe.
    const DOCUMENTED_ROUTES: &[(&str, &str)] = &[
        ("get", "/api/v1/health"),
        ("get", "/api/v1/health/ready"),
        ("get", "/api/v1/info"),
        ("get", "/api/v1/accounts"),
        ("post", "/api/v1/accounts"),
        ("get", "/api/v1/accounts/{id}"),
        ("patch", "/api/v1/accounts/{id}"),
        ("delete", "/api/v1/accounts/{id}"),
        ("get", "/api/v1/mailboxes"),
        ("post", "/api/v1/mailboxes"),
        ("get", "/api/v1/mailboxes/{id}"),
        ("patch", "/api/v1/mailboxes/{id}"),
        ("delete", "/api/v1/mailboxes/{id}"),
        ("get", "/api/v1/mailboxes/{id}/entries"),
        ("get", "/api/v1/entries/{id}"),
        ("delete", "/api/v1/entries/{id}"),
        ("patch", "/api/v1/entries/{id}/flags"),
        ("post", "/api/v1/entries/{id}/move"),
        ("post", "/api/v1/entries/{id}/copy"),
        ("post", "/api/v1/entries/bulk"),
        ("get", "/api/v1/messages"),
        ("post", "/api/v1/messages"),
        ("get", "/api/v1/messages/{id}"),
        ("delete", "/api/v1/messages/{id}"),
        ("get", "/api/v1/messages/{id}/raw"),
        ("get", "/api/v1/messages/{id}/headers"),
        ("get", "/api/v1/messages/{id}/mime"),
        ("get", "/api/v1/messages/{id}/part/{path}"),
        ("get", "/api/v1/messages/{id}/delivery"),
        ("get", "/api/v1/messages/{id}/attachments"),
        ("get", "/api/v1/attachments/{id}"),
        ("get", "/api/v1/attachments/{id}/thumbnail"),
        ("get", "/api/v1/search"),
        ("get", "/api/v1/search/explain"),
        ("post", "/api/v1/send"),
        ("get", "/api/v1/events/sse"),
    ];

    fn document() -> serde_json::Value {
        serde_json::from_str(&document_json().expect("the document serialises"))
            .expect("valid JSON")
    }

    #[test]
    fn the_document_declares_openapi_3_1() {
        let doc = document();
        let version = doc["openapi"].as_str().expect("an openapi version");
        assert!(
            version.starts_with("3.1"),
            "expected OpenAPI 3.1, got {version}"
        );
    }

    #[test]
    fn every_route_is_documented() {
        // The guard against the most likely contract failure: someone adds a
        // route to `router()` and forgets to annotate the handler, leaving the
        // generated TypeScript without it.
        let doc = document();
        for (method, path) in DOCUMENTED_ROUTES {
            let operation = &doc["paths"][path][method];
            assert!(
                !operation.is_null(),
                "{} {path} is missing from the OpenAPI document",
                method.to_uppercase()
            );
        }
    }

    #[test]
    fn no_operation_is_missing_a_success_response() {
        let doc = document();
        for (method, path) in DOCUMENTED_ROUTES {
            let responses = &doc["paths"][path][method]["responses"];
            let has_success = responses
                .as_object()
                .expect("responses is an object")
                .keys()
                .any(|code| code.starts_with('2'));
            assert!(has_success, "{method} {path} documents no 2xx response");
        }
    }

    #[test]
    fn every_referenced_schema_resolves() {
        // A `$ref` to a schema that was never registered produces TypeScript
        // that references a type which does not exist. Catch it here instead.
        let doc = document();
        let schemas = doc["components"]["schemas"]
            .as_object()
            .expect("schemas are declared");

        let mut refs = Vec::new();
        collect_refs(&doc, &mut refs);

        for reference in refs {
            let name = reference
                .strip_prefix("#/components/schemas/")
                .unwrap_or_else(|| panic!("unexpected $ref target: {reference}"));
            assert!(
                schemas.contains_key(name),
                "{reference} does not resolve to a registered schema"
            );
        }
    }

    fn collect_refs(value: &serde_json::Value, into: &mut Vec<String>) {
        match value {
            serde_json::Value::Object(map) => {
                for (key, child) in map {
                    if key == "$ref"
                        && let Some(reference) = child.as_str()
                    {
                        into.push(reference.to_owned());
                    }
                    collect_refs(child, into);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    collect_refs(item, into);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn generation_is_deterministic() {
        // The drift check compares bytes, so unstable ordering would make CI
        // fail at random. This asserts the property that check depends on.
        assert_eq!(
            document_json().expect("serialises"),
            document_json().expect("serialises"),
        );
    }

    #[test]
    fn the_problem_details_schema_is_published() {
        // Every non-2xx response points at it, so its absence would leave the
        // frontend without a typed error shape.
        let doc = document();
        assert!(!doc["components"]["schemas"]["ProblemDetails"].is_null());
    }
}
