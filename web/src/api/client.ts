/**
 * The one place Sandesh talks HTTP.
 *
 * Every request in the application goes through `request()`. Components never
 * call `fetch` (§29): that keeps error translation, the base URL and the
 * problem-details contract in a single auditable place rather than repeated
 * with small variations across features.
 *
 * Types come from `../generated/api`, which is generated from the Rust API by
 * `cargo xtask generate-api`. Nothing here restates a backend shape by hand, so
 * a contract change surfaces as a TypeScript error rather than as a runtime
 * surprise.
 */

import type { components } from '../generated/api'

type Schemas = components['schemas']

export type Account = Schemas['AccountDto']
export type Address = Schemas['AddressDto']
export type Attachment = Schemas['AttachmentDto']
export type Delivery = Schemas['DeliveryDto']
export type Connection = Schemas['ConnectionDto']
export type LogLine = Schemas['LogLineDto']
export type Diagnostic = Schemas['DiagnosticDto']
export type Entry = Schemas['EntryDto']
export type EntryRef = Schemas['EntryRef']
export type Header = Schemas['HeaderDto']
export type ImportResponse = Schemas['ImportResponse']
export type HeaderAnalysis = Schemas['HeaderAnalysisDto']
export type LinkReport = Schemas['LinkReportDto']
export type ExtractedLink = Schemas['ExtractedLinkDto']
export type ReceivedHop = Schemas['ReceivedHopDto']
export type DkimSignature = Schemas['DkimSignatureDto']
export type AuthResult = Schemas['AuthResultDto']
export type SpfRecord = Schemas['SpfRecordDto']
export type SpamHeader = Schemas['SpamHeaderDto']
export type Faults = Schemas['FaultsDto']
export type FaultRule = Schemas['FaultRuleDto']
export type WebhookEndpoint = Schemas['WebhookEndpointDto']
export type NewWebhook = Schemas['NewWebhookDto']
export type OutboxEntry = Schemas['OutboxEntryDto']
export type Health = Schemas['HealthDto']
export type Info = Schemas['InfoDto']
export type Mailbox = Schemas['MailboxDto']
export type MessageDetail = Schemas['MessageDetail']
export type MessageSummary = Schemas['MessageSummary']
export type MimePart = Schemas['MimePartDto']
export type MimeTree = Schemas['MimeTree']
export type ProblemDetails = Schemas['ProblemDetails']
export type Ready = Schemas['ReadyDto']
export type SendMessageRequest = Schemas['SendMessageRequest']
export type SendMessageResponse = Schemas['SendMessageResponse']
export type BulkAction = Schemas['BulkAction']
export type BulkEntriesRequest = Schemas['BulkEntriesRequest']
export type BulkEntriesResponse = Schemas['BulkEntriesResponse']
export type SearchExplainResponse = Schemas['SearchExplainResponse']

// Workspaces — multi-project storage isolation (§workspace).
// The generated `api.ts` is regenerated via `cargo xtask generate-api`;
// until then the shapes are declared inline, mirroring `dto.rs` exactly.
export interface Workspace {
  /** Slug identifier, e.g. "default" or "billing-service". */
  id: string
  /** Human-readable display name. */
  name: string
  /** Optional free-text description. */
  description: string | null
  /** Creation timestamp (RFC 3339 UTC). */
  created_at: string
  /** Whether this workspace is currently active. */
  is_active: boolean
  /** Whether authentication is required for SMTP/IMAP/POP3 in this workspace. */
  auth_required: boolean
  /** Total message count in this workspace's database. */
  message_count: number
  /** Combined size on disk (database + WAL + blobs) in bytes. */
  size_bytes: number
  /** Absolute path to the SQLite database file. */
  database_path: string
}

export interface PurgeResult {
  workspace_id: string
  deleted_messages: number
  deleted_traces: number
  reclaimed_bytes: number
  purged_at: string
}

export type ListResponse<T> = {
  items: T[]
  next_cursor: string | null
  total_estimate: number
}

/**
 * The API base path.
 *
 * Same-origin in every deployment: in production the Rust binary serves both
 * the assets and the API, and in development Vite proxies `/api` to the
 * backend. Neither case needs an absolute URL, which is why no component ever
 * mentions a host or port (§43).
 */
export const API_BASE = '/api/v1'

/** A failed request, carrying the server's problem details when it sent any. */
export class ApiError extends Error {
  readonly status: number
  /** The stable slug from the problem `type`, e.g. `not-found`. */
  readonly slug: string
  readonly detail: string
  /** Whether retrying the identical request could plausibly succeed. */
  readonly retryable: boolean

  constructor(status: number, slug: string, title: string, detail: string) {
    super(title)
    this.name = 'ApiError'
    this.status = status
    this.slug = slug
    this.detail = detail
    // A 4xx is the client's fault and will fail identically on retry; a 5xx,
    // a 503 or a transport failure may not. Offering "Retry" on a 404 wastes
    // the user's time and teaches them to distrust the button.
    this.retryable = status === 0 || status >= 500
  }

  /** True when the requested entity does not exist. */
  get notFound(): boolean {
    return this.status === 404
  }
}

/** The shape of a network failure, which never reaches the server at all. */
const OFFLINE = new ApiError(
  0,
  'unreachable',
  'Sandesh is unreachable',
  'The request did not reach the server. Check that Sandesh is still running.',
)

function problemToError(status: number, body: unknown): ApiError {
  // RFC 9457: `type` is a URI whose last segment is the stable slug clients
  // branch on. `title` is prose and may be reworded, so it is never matched.
  if (body && typeof body === 'object' && 'title' in body) {
    const problem = body as ProblemDetails
    const slug = String(problem.type ?? '').split('/').filter(Boolean).pop() ?? 'unknown'
    return new ApiError(status, slug, problem.title ?? 'Request failed', problem.detail ?? '')
  }
  return new ApiError(status, 'unknown', 'Request failed', `The server returned ${status}.`)
}

async function parseFailure(response: Response): Promise<ApiError> {
  try {
    return problemToError(response.status, await response.json())
  } catch {
    // A non-JSON error body is still a real failure; do not mask it.
    return new ApiError(
      response.status,
      'unknown',
      'Request failed',
      `The server returned ${response.status}.`,
    )
  }
}

async function send(path: string, init?: RequestInit): Promise<Response> {
  let response: Response
  try {
    response = await fetch(`${API_BASE}${path}`, {
      ...init,
      headers: { Accept: 'application/json', ...init?.headers },
    })
  } catch {
    throw OFFLINE
  }

  if (!response.ok) {
    throw await parseFailure(response)
  }

  return response
}

/** Performs a request and decodes a JSON response. */
export async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await send(path, init)
  return (await response.json()) as T
}

/** Performs a request expecting no response body. */
export async function requestEmpty(path: string, init?: RequestInit): Promise<void> {
  await send(path, init)
}

/** Fetches a response as text, for raw source and decoded MIME parts. */
export async function requestText(path: string, init?: RequestInit): Promise<string> {
  const response = await send(path, { ...init, headers: { Accept: '*/*', ...init?.headers } })
  return response.text()
}

/** Builds a query string, omitting empty values so URLs stay readable. */
export function query(params: Record<string, string | number | undefined | null>): string {
  const search = new URLSearchParams()
  for (const [key, value] of Object.entries(params)) {
    if (value !== undefined && value !== null && value !== '') {
      search.set(key, String(value))
    }
  }
  const rendered = search.toString()
  return rendered ? `?${rendered}` : ''
}
