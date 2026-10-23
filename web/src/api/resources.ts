/**
 * The API surface, one function per endpoint.
 *
 * These mirror the routes in `crates/sandesh-api/src/lib.rs` exactly. Anything
 * the backend does not expose is absent here rather than faked, which is what
 * keeps the UI honest about what Sandesh can actually do (§59).
 */

import {
  type Account,
  type Attachment,
  type BulkEntriesRequest,
  type BulkEntriesResponse,
  type Connection,
  type Delivery,
  type Entry,
  type Faults,
  type Header,
  type ImportResponse,
  type HeaderAnalysis,
  type LinkReport,
  type Info,
  type ListResponse,
  type LogLine,
  type Mailbox,
  type MessageDetail,
  type MessageSummary,
  type MimeTree,
  type NewWebhook,
  type OutboxEntry,
  type PurgeResult,
  type Ready,
  type SearchExplainResponse,
  type SendMessageRequest,
  type SendMessageResponse,
  type WebhookEndpoint,
  type Workspace,
  API_BASE,
  query,
  request,
  requestEmpty,
  requestText,
} from './client'

/** Page size for the message list. The backend caps this server-side. */
export const PAGE_SIZE = 50

export const health = {
  ready: () => request<Ready>('/health/ready'),
  info: () => request<Info>('/info'),
}

/**
 * Injected SMTP faults (ADR-084).
 *
 * Read-only here: the UI reports what is configured but does not configure it.
 * Setting a fault is a deliberate act performed from a test suite or the API
 * console, and a button that could break mail delivery from the message list
 * would be a trap rather than a feature.
 */
export const faults = {
  list: () => request<Faults>('/faults'),
}

/**
 * Webhooks and the outbox (ADR-085, ADR-086).
 *
 * Writable here, unlike `faults`: registering an endpoint cannot break mail
 * delivery -- the worker sends after commit, so a misconfigured endpoint fails
 * its own notifications and nothing else. A fault, by contrast, changes what
 * SMTP clients see, which is why that one is read-only from the UI.
 */
export const webhooks = {
  list: () => request<WebhookEndpoint[]>('/webhooks'),
  create: (payload: NewWebhook) =>
    request<WebhookEndpoint>('/webhooks', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(payload),
    }),
  remove: (id: string) =>
    requestEmpty(`/webhooks/${encodeURIComponent(id)}`, { method: 'DELETE' }),
  outbox: (limit = PAGE_SIZE) =>
    request<OutboxEntry[]>(`/webhooks/outbox${query({ limit })}`),
}

export const accounts = {
  list: () => request<ListResponse<Account>>('/accounts'),
  get: (id: string) => request<Account>(`/accounts/${encodeURIComponent(id)}`),
  create: (payload: {
    username: string
    password?: string | undefined
    addresses?: string[] | undefined
    is_catch_all?: boolean | undefined
  }) =>
    request<Account>('/accounts', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(payload),
    }),
  patch: (
    id: string,
    payload: {
      password?: string | undefined
      add_addresses?: string[] | undefined
      remove_addresses?: string[] | undefined
      is_catch_all?: boolean | undefined
    },
  ) =>
    request<Account>(`/accounts/${encodeURIComponent(id)}`, {
      method: 'PATCH',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(payload),
    }),
  delete: (id: string) =>
    requestEmpty(`/accounts/${encodeURIComponent(id)}`, { method: 'DELETE' }),
}

export const mailboxes = {
  list: (accountId?: string) =>
    request<ListResponse<Mailbox>>(`/mailboxes${query({ account_id: accountId })}`),
  get: (id: string) => request<Mailbox>(`/mailboxes/${encodeURIComponent(id)}`),
  entries: (id: string, options: { limit?: number; cursor?: string } = {}) =>
    request<ListResponse<Entry>>(
      `/mailboxes/${encodeURIComponent(id)}/entries${query({
        limit: options.limit ?? PAGE_SIZE,
        cursor: options.cursor,
      })}`,
    ),

  /** Creates a new mailbox under the given (or default) account. */
  create: (payload: { name: string; account_id?: string }) =>
    request<Mailbox>('/mailboxes', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(payload),
    }),

  /** Renames a mailbox or toggles its subscription flag. */
  patch: (id: string, payload: { name?: string; subscribed?: boolean }) =>
    request<Mailbox>(`/mailboxes/${encodeURIComponent(id)}`, {
      method: 'PATCH',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(payload),
    }),

  /** Deletes a mailbox and all its placements. INBOX cannot be deleted (400). */
  delete: (id: string) =>
    requestEmpty(`/mailboxes/${encodeURIComponent(id)}`, { method: 'DELETE' }),
}

export const entries = {
  get: (id: string) => request<Entry>(`/entries/${encodeURIComponent(id)}`),

  /** Adds and removes flags in one request; maps to IMAP `STORE`. */
  setFlags: (id: string, patch: { add?: string[]; remove?: string[] }) =>
    request<Entry>(`/entries/${encodeURIComponent(id)}/flags`, {
      method: 'PATCH',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ add: patch.add ?? [], remove: patch.remove ?? [] }),
    }),

  /** Moves a placement to another mailbox. */
  move: (
    id: string,
    target: { target_mailbox_id?: string; mailbox_name?: string },
  ) =>
    request<Entry>(`/entries/${encodeURIComponent(id)}/move`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(target),
    }),

  /**
   * Removes one placement (moves to Trash, or permanently purges if permanent is true or already in Trash).
   */
  remove: (id: string, permanent: boolean = false) =>
    requestEmpty(
      `/entries/${encodeURIComponent(id)}${permanent ? '?permanent=true' : ''}`,
      { method: 'DELETE' },
    ),

  /** Performs bulk operations across multiple placements. */
  bulk: (payload: BulkEntriesRequest) =>
    request<BulkEntriesResponse>('/entries/bulk', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(payload),
    }),
}

export const messages = {
  list: (options: { mailboxId?: string; accountId?: string; cursor?: string; limit?: number } = {}) =>
    request<ListResponse<MessageSummary>>(
      `/messages${query({
        mailbox_id: options.mailboxId,
        account_id: options.accountId,
        cursor: options.cursor,
        limit: options.limit ?? PAGE_SIZE,
      })}`,
    ),

  get: (id: string) => request<MessageDetail>(`/messages/${encodeURIComponent(id)}`),
  // A bare array, not a list envelope: headers are never paginated, so the
  // endpoint returns them all. The contract test in `tests/http_surface.rs`
  // keeps this shape honest.
  headers: (id: string) =>
    request<Header[]>(`/messages/${encodeURIComponent(id)}/headers`),

  // The interpreted view of the same headers: the Received chain as a
  // chronological hop list, plus whatever authentication headers upstream left
  // behind. Nothing here is verified; the response says so in `disclaimer`.
  headerAnalysis: (id: string) =>
    request<HeaderAnalysis>(`/messages/${encodeURIComponent(id)}/headers/analysis`),
  // Every URL the message's HTML references, classified. Nothing is fetched:
  // the server resolves no URL, and the response says so in `disclaimer`.
  links: (id: string) => request<LinkReport>(`/messages/${encodeURIComponent(id)}/links`),

  mime: (id: string) => request<MimeTree>(`/messages/${encodeURIComponent(id)}/mime`),
  delivery: (id: string) => request<Delivery>(`/messages/${encodeURIComponent(id)}/delivery`),

  /** The exact stored octets. */
  raw: (id: string) => requestText(`/messages/${encodeURIComponent(id)}/raw`),

  /** One decoded MIME part, as text. The root part is addressed as `root`. */
  part: (id: string, path: string) =>
    requestText(`/messages/${encodeURIComponent(id)}/part/${encodeURIComponent(path || 'root')}`),

  /**
   * The URL of a part's bytes.
   *
   * Returned as a URL rather than as fetched bytes because attachments and
   * images belong in `src`/`href`, where the browser can stream them instead of
   * the application buffering them in memory (§56).
   */
  partUrl: (id: string, path: string) =>
    `${API_BASE}/messages/${encodeURIComponent(id)}/part/${encodeURIComponent(path || 'root')}`,

  /** Ingests raw RFC 822 / MIME message octets directly into storage. */
  inject: (rawBytes: BodyInit) =>
    request<MessageSummary>('/messages', {
      method: 'POST',
      headers: { 'Content-Type': 'message/rfc822' },
      body: rawBytes,
    }),

  /** Lists attachment metadata for a message. */
  attachments: (id: string) =>
    request<ListResponse<Attachment>>(`/messages/${encodeURIComponent(id)}/attachments`),

  /** Composes and delivers an email message. */
  send: (payload: SendMessageRequest) =>
    request<SendMessageResponse>('/send', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(payload),
    }),

  /**
   * Delivers a stored message again, as a new message.
   *
   * The replay carries a fresh `Message-ID` and an `X-Sandesh-Replay-Of`
   * header naming the original, so the two are distinguishable to anything
   * downstream that deduplicates.
   */
  replay: (id: string) =>
    request<MessageSummary>(`/messages/${encodeURIComponent(id)}/replay`, {
      method: 'POST',
    }),
}

/** Bulk movement of messages in and out, as mbox or a single `.eml`. */
export const transfer = {
  /**
   * The URL of an export, for the browser to download directly.
   *
   * A URL rather than fetched bytes for the same reason attachments are: the
   * browser streams the file to disk instead of the application holding a
   * whole corpus in memory.
   */
  exportUrl: (
    options: {
      format?: 'mbox' | 'eml'
      accountId?: string | undefined
      mailboxId?: string | undefined
      limit?: number | undefined
    } = {},
  ) =>
    `${API_BASE}/export${query({
      format: options.format ?? 'mbox',
      account_id: options.accountId,
      mailbox_id: options.mailboxId,
      limit: options.limit,
    })}`,

  /** Imports an mbox file, or a single message, into storage with optional target mailbox placement. */
  import: (file: BodyInit, format: 'mbox' | 'eml' = 'mbox', mailboxId?: string | undefined) =>
    request<ImportResponse>(`/import${query({ format, mailbox_id: mailboxId })}`, {
      method: 'POST',
      headers: { 'Content-Type': format === 'mbox' ? 'application/mbox' : 'message/rfc822' },
      body: file,
    }),
}

/**
 * Per-connection protocol transcripts (Phase 7 A2).
 *
 * The read side of ADR-066. Both calls return `503` when tracing is disabled,
 * which the view reports as a configuration state rather than as an error —
 * "no transcripts recorded" and "nothing has connected yet" are different
 * answers to someone wondering where their trace went.
 */
export const connections = {
  list: (params: { protocol?: string; limit?: number } = {}) =>
    request<ListResponse<Connection>>(`/connections${query(params)}`),
  get: (id: string) => request<Connection>(`/connections/${encodeURIComponent(id)}`),
}

export const logs = {
  /** The bounded backlog, oldest first. */
  list: (params: { level?: string; target?: string; limit?: number } = {}) =>
    request<ListResponse<LogLine>>(`/logs${query(params)}`),
  /**
   * The live stream's URL.
   *
   * Returned rather than opened here: `EventSource` is a long-lived
   * connection whose lifetime belongs to the component that shows it, not to
   * a fetch helper that returns a promise.
   */
  streamUrl: (params: { level?: string; target?: string } = {}) =>
    `${API_BASE}/logs/stream${query(params)}`,
}

export const attachments = {
  /** URL for direct attachment download or inline previewing. */
  url: (id: string, inline: boolean = false) =>
    `${API_BASE}/attachments/${encodeURIComponent(id)}${inline ? '?inline=true' : ''}`,

  /** URL for server-generated image thumbnail. */
  thumbnailUrl: (id: string) =>
    `${API_BASE}/attachments/${encodeURIComponent(id)}/thumbnail`,

  /** Downloads decoded attachment bytes as a Blob. */
  download: (id: string) => request<Blob>(`/attachments/${encodeURIComponent(id)}`),
}

export const search = {
  messages: (options: {
    q?: string | undefined
    mailboxId?: string | undefined
    accountId?: string | undefined
    cursor?: string | undefined
    limit?: number | undefined
  } = {}) =>
    request<ListResponse<MessageSummary>>(
      `/search${query({
        q: options.q,
        mailbox_id: options.mailboxId,
        account_id: options.accountId,
        cursor: options.cursor,
        limit: options.limit ?? PAGE_SIZE,
      })}`,
    ),

  explain: (q: string) =>
    request<SearchExplainResponse>(`/search/explain${query({ q })}`),
}

/**
 * Workspace management — isolated per-project databases (ADR-workspace).
 *
 * Switching the active workspace takes effect immediately for all subsequent
 * API reads and SMTP deliveries without restarting the server. The router
 * holds one `WorkspaceBundle` per opened workspace; `activate` tells it which
 * one to use for the next request.
 *
 * The `default` workspace can never be deleted; every other operation is
 * available on it.
 */
export const workspaces = {
  /** Lists all workspaces with live message counts and disk sizes. */
  list: () => request<Workspace[]>('/workspaces'),

  /** Fetches a single workspace by slug. */
  get: (id: string) => request<Workspace>(`/workspaces/${encodeURIComponent(id)}`),

  /**
   * Creates a new workspace.
   *
   * If `set_active` is true the server immediately switches to it, so the
   * caller gets a workspace that is already live — no separate activate call.
   */
  create: (payload: {
    name: string
    id?: string | undefined
    description?: string | undefined
    clone_from?: string | undefined
    password?: string | undefined
    auth_required?: boolean | undefined
    initial_account?:
      | {
          username?: string | undefined
          addresses?: string[] | undefined
          is_catch_all?: boolean | undefined
        }
      | undefined
    set_active?: boolean | undefined
  }) =>
    request<Workspace>('/workspaces', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(payload),
    }),

  /**
   * Updates workspace properties (name, description, auth requirement).
   */
  update: (
    id: string,
    payload: {
      name?: string | undefined
      description?: string | null | undefined
      auth_required?: boolean | undefined
    },
  ) =>
    request<Workspace>(`/workspaces/${encodeURIComponent(id)}`, {
      method: 'PATCH',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(payload),
    }),

  /**
   * Switches the active workspace.
   *
   * The router evicts the previous workspace's cached bundle and opens the
   * new one on the first subsequent request.
   */
  activate: (id: string) =>
    request<Workspace>(`/workspaces/${encodeURIComponent(id)}/activate`, {
      method: 'POST',
    }),

  /**
   * Purges data in a workspace.
   *
   * Mode `"mail_and_traces"` (default): deletes messages, connection traces,
   * and optionally webhook logs, then VACUUMs the database.
   * Mode `"factory_reset"`: everything above plus accounts and rules.
   */
  purge: (
    id: string,
    options: { mode?: 'mail_and_traces' | 'factory_reset'; clear_traces?: boolean; clear_webhooks?: boolean } = {},
  ) =>
    request<PurgeResult>(`/workspaces/${encodeURIComponent(id)}/purge`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(options),
    }),

  /** Purges the currently active workspace (shorthand for `purge(activeId)`). */
  purgeActive: (
    options: { mode?: 'mail_and_traces' | 'factory_reset'; clear_traces?: boolean; clear_webhooks?: boolean } = {},
  ) =>
    request<PurgeResult>('/purge', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(options),
    }),

  /**
   * Deletes a workspace and removes its database file from disk.
   *
   * The `default` workspace cannot be deleted (400). The server evicts the
   * cached bundle so no stale connection pool remains.
   */
  delete: (id: string) =>
    requestEmpty(`/workspaces/${encodeURIComponent(id)}`, { method: 'DELETE' }),
}
