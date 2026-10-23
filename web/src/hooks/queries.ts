/**
 * Server state (§57).
 *
 * Everything the backend owns is read through these hooks; nothing here holds
 * UI state. The split matters: server state is cached, refetched and
 * invalidated by events, while UI state (selection, theme, open tab) belongs to
 * the URL or to a component and never needs a cache.
 */

import { useMemo } from 'react'

import {
  useInfiniteQuery,
  useMutation,
  useQuery,
  useQueryClient,
} from '@tanstack/react-query'

import {
  accounts,
  connections,
  entries,
  faults,
  health,
  mailboxes,
  messages,
  search,
  transfer,
  webhooks,
  workspaces,
} from '../api/resources'
import type { BulkEntriesRequest, NewWebhook, SendMessageRequest } from '../api/client'

/** Query keys, centralised so an invalidation cannot miss a related query. */
export const keys = {
  info: () => ['info'] as const,
  ready: () => ['ready'] as const,
  accounts: () => ['accounts'] as const,
  mailboxes: () => ['mailboxes'] as const,
  messages: () => ['messages'] as const,
  messageList: (mailboxId: string | undefined) => ['messages', { mailboxId }] as const,
  message: (id: string) => ['message', id] as const,
  headers: (id: string) => ['message', id, 'headers'] as const,
  headerAnalysis: (id: string) => ['message', id, 'headers', 'analysis'] as const,
  links: (id: string) => ['message', id, 'links'] as const,
  mime: (id: string) => ['message', id, 'mime'] as const,
  raw: (id: string) => ['message', id, 'raw'] as const,
  delivery: (id: string) => ['message', id, 'delivery'] as const,
  search: (q: string, mailboxId?: string) => ['search', { q, mailboxId }] as const,
  searchExplain: (q: string) => ['search-explain', q] as const,
  connections: (protocol?: string) => ['connections', { protocol }] as const,
  connection: (id: string) => ['connection', id] as const,
  faults: () => ['faults'] as const,
  webhooks: () => ['webhooks'] as const,
  outbox: () => ['webhook-outbox'] as const,
  workspaces: () => ['workspaces'] as const,
  workspace: (id: string) => ['workspace', id] as const,
}

export function useInfo() {
  // Connection details change only on restart, so this need not be refetched.
  return useQuery({ queryKey: keys.info(), queryFn: health.info, staleTime: Infinity })
}

/**
 * Whether any SMTP fault is currently injected (ADR-084).
 *
 * Deliberately not read from `useInfo`, which is cached with `staleTime:
 * Infinity` because connection details change only on restart. Faults are the
 * one field of `/info` that changes while the server runs, so sharing that
 * cache would leave the indicator showing a state that ended minutes ago —
 * precisely the "why is delivery broken?" confusion the indicator exists to
 * prevent.
 */
export function useFaults() {
  return useQuery({
    queryKey: keys.faults(),
    queryFn: faults.list,
    refetchInterval: 15_000,
  })
}

/** The registered webhook endpoints (ADR-085). */
export function useWebhooks() {
  return useQuery({ queryKey: keys.webhooks(), queryFn: webhooks.list })
}

/**
 * Recent notifications and their outcomes.
 *
 * Polled, because the worker delivers on its own schedule (ADR-086): an entry
 * enqueued by a delivery moves from pending to delivered without any action in
 * the UI, and a view that only refetched on navigation would show a queue that
 * had in fact drained.
 */
export function useOutbox() {
  return useQuery({
    queryKey: keys.outbox(),
    queryFn: () => webhooks.outbox(),
    refetchInterval: 5_000,
  })
}

export function useRegisterWebhook() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: (payload: NewWebhook) => webhooks.create(payload),
    onSuccess: () => {
      void client.invalidateQueries({ queryKey: keys.webhooks() })
    },
  })
}

export function useRemoveWebhook() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: (id: string) => webhooks.remove(id),
    onSuccess: () => {
      // The outbox too: removing an endpoint cascades to its queued entries,
      // so a stale list would show notifications for a destination that is
      // gone.
      void client.invalidateQueries({ queryKey: keys.webhooks() })
      void client.invalidateQueries({ queryKey: keys.outbox() })
    },
  })
}

export function useReady() {
  return useQuery({ queryKey: keys.ready(), queryFn: health.ready, refetchInterval: 30_000 })
}

export function useAccounts() {
  return useQuery({ queryKey: keys.accounts(), queryFn: accounts.list })
}

export function useMailboxes() {
  return useQuery({ queryKey: keys.mailboxes(), queryFn: () => mailboxes.list() })
}

/**
 * The message list, paged by the backend's UID cursor.
 *
 * An infinite query rather than a plain one, because the list is the only view
 * that can grow without bound: a CI run delivering ten thousand messages is the
 * case the 10,000-message target names. Pages accumulate in one cache entry, so
 * scrolling never refetches what is already held, and the virtualised list
 * renders only the rows on screen.
 *
 * `next_cursor` being absent is what "no more pages" means — the count, not a
 * separate total, is the end signal, which keeps this honest under concurrent
 * insertion (the cursor is stable where an offset would not be).
 */
export function useMessages(mailboxId: string | undefined) {
  const query = useInfiniteQuery({
    queryKey: keys.messageList(mailboxId),
    queryFn: ({ pageParam }) =>
      messages.list({
        ...(mailboxId ? { mailboxId } : {}),
        ...(pageParam ? { cursor: pageParam } : {}),
      }),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (last) => last.next_cursor ?? undefined,
  })

  // Flattened once here rather than at each call site: every consumer wants the
  // rows, and re-flattening per render would defeat the memoised filtering
  // downstream.
  const items = useMemo(
    () => (query.data?.pages ?? []).flatMap((page) => page.items),
    [query.data?.pages],
  )

  return { ...query, items }
}

export function useMessage(id: string | undefined) {
  return useQuery({
    queryKey: keys.message(id ?? ''),
    queryFn: () => messages.get(id ?? ''),
    enabled: Boolean(id),
  })
}

export function useHeaders(id: string | undefined) {
  return useQuery({
    queryKey: keys.headers(id ?? ''),
    queryFn: () => messages.headers(id ?? ''),
    enabled: Boolean(id),
  })
}

export function useHeaderAnalysis(id: string | undefined, enabled: boolean) {
  return useQuery({
    queryKey: keys.headerAnalysis(id ?? ''),
    queryFn: () => messages.headerAnalysis(id ?? ''),
    // Only fetched once the Headers tab is open: it re-parses the same headers
    // the flat list already showed, so it is never needed first.
    enabled: Boolean(id) && enabled,
  })
}

export function useLinks(id: string | undefined, enabled: boolean) {
  return useQuery({
    queryKey: keys.links(id ?? ''),
    queryFn: () => messages.links(id ?? ''),
    // Only fetched once the Links tab is open: it decodes every HTML part,
    // which the other tabs never need.
    enabled: Boolean(id) && enabled,
  })
}

export function useMime(id: string | undefined) {
  return useQuery({
    queryKey: keys.mime(id ?? ''),
    queryFn: () => messages.mime(id ?? ''),
    enabled: Boolean(id),
  })
}

export function useRaw(id: string | undefined, enabled: boolean) {
  return useQuery({
    queryKey: keys.raw(id ?? ''),
    queryFn: () => messages.raw(id ?? ''),
    // Raw source can be megabytes, so it is fetched only when actually shown.
    enabled: Boolean(id) && enabled,
  })
}

export function useDelivery(id: string | undefined, enabled: boolean) {
  return useQuery({
    queryKey: keys.delivery(id ?? ''),
    queryFn: () => messages.delivery(id ?? ''),
    enabled: Boolean(id) && enabled,
    retry: false,
  })
}

/**
 * Recent connection transcripts, newest first.
 *
 * Polled rather than pushed: a record is written when its socket closes, and
 * the event bus carries mail events, not connection lifecycle. Polling is the
 * honest option until there is an event to subscribe to — inventing one for
 * the view would be a backend change disguised as a UI feature.
 */
export function useConnections(protocol?: string) {
  return useQuery({
    queryKey: keys.connections(protocol),
    queryFn: () => connections.list(protocol ? { protocol } : {}),
    // Adaptive: an open connection's transcript now grows while the socket is
    // live (Phase 7 Pass B), and five seconds between frames does not read as
    // live. When everything is closed the transcripts are immutable, so there
    // is nothing a faster poll could discover — five seconds is then a
    // liveness check for *new* connections, not for changed ones.
    refetchInterval: (query) =>
      query.state.data?.items.some((connection) => connection.closed_at === null)
        ? 1000
        : 5000,
    retry: false,
  })
}

/** One connection's full transcript. */
export function useConnection(id: string | undefined) {
  return useQuery({
    queryKey: keys.connection(id ?? ''),
    queryFn: () => connections.get(id ?? ''),
    enabled: Boolean(id),
    retry: false,
  })
}

/**
 * Adds and removes flags on one placement.
 *
 * Not optimistic: the WebSocket reports the authoritative change within
 * milliseconds, and an optimistic flip that the server then rejects would show
 * the developer a state their mail server is not actually in.
 */
export function useSetFlags() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: ({ entryId, add, remove }: { entryId: string; add?: string[]; remove?: string[] }) =>
      entries.setFlags(entryId, { ...(add ? { add } : {}), ...(remove ? { remove } : {}) }),
    onSuccess: () => {
      void client.invalidateQueries({ queryKey: keys.messages() })
      void client.invalidateQueries({ queryKey: keys.mailboxes() })
      // The open message carries its own flags and lives under a different key
      // (`['message', id]`, not `['messages']`), so it must be invalidated
      // explicitly or the viewer keeps showing the flags it loaded with.
      void client.invalidateQueries({ queryKey: ['message'] })

    },
  })
}

/** Removes one placement (moves to Trash, or purges if already in Trash / permanent is true). */
export function useDeleteEntry() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: (arg: string | { entryId: string; permanent?: boolean }) => {
      const entryId = typeof arg === 'string' ? arg : arg.entryId
      const permanent = typeof arg === 'string' ? false : (arg.permanent ?? false)
      return entries.remove(entryId, permanent)
    },
    onSuccess: () => {
      void client.invalidateQueries({ queryKey: keys.messages() })
      void client.invalidateQueries({ queryKey: keys.mailboxes() })
    },
  })
}

/** Moves an entry to another mailbox. */
export function useMoveEntry() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: ({
      entryId,
      targetMailboxId,
      mailboxName,
    }: {
      entryId: string
      targetMailboxId?: string
      mailboxName?: string
    }) => {
      const payload: { target_mailbox_id?: string; mailbox_name?: string } = {}
      if (targetMailboxId !== undefined) payload.target_mailbox_id = targetMailboxId
      if (mailboxName !== undefined) payload.mailbox_name = mailboxName
      return entries.move(entryId, payload)
    },
    onSuccess: () => {
      void client.invalidateQueries({ queryKey: keys.messages() })
      void client.invalidateQueries({ queryKey: keys.mailboxes() })
    },
  })
}

/** Composes and delivers an email message. */
export function useSendMessage() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: (payload: SendMessageRequest) => messages.send(payload),
    onSuccess: () => {
      void client.invalidateQueries({ queryKey: keys.messages() })
      void client.invalidateQueries({ queryKey: keys.mailboxes() })
    },
  })
}

/**
 * Delivers a stored message again (Phase 8 A5).
 *
 * Invalidates the message list because the replay arrives as a new message —
 * it is not an edit of the one being viewed.
 */
export function useReplayMessage() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: (messageId: string) => messages.replay(messageId),
    onSuccess: () => {
      void client.invalidateQueries({ queryKey: keys.messages() })
      void client.invalidateQueries({ queryKey: keys.mailboxes() })
    },
  })
}

/** Imports an mbox file, or a single message, into storage (Phase 8 A5). */
export function useImportMessages() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: ({
      file,
      format,
      mailboxId,
    }: {
      file: File
      format: 'mbox' | 'eml'
      mailboxId?: string | undefined
    }) => transfer.import(file, format, mailboxId),
    onSuccess: () => {
      void client.invalidateQueries({ queryKey: keys.messages() })
      void client.invalidateQueries({ queryKey: keys.mailboxes() })
      void client.invalidateQueries({ queryKey: ['workspaces'] })
    },
  })
}

/** Searches messages using server filter query syntax. */
export function useSearchMessages(q: string, mailboxId?: string) {
  return useQuery({
    queryKey: keys.search(q, mailboxId),
    queryFn: () => search.messages({ q, mailboxId }),
    enabled: Boolean(q.trim()),
  })
}

/** Explains the domain search plan compiled for a filter expression. */
export function useSearchExplain(q: string) {
  return useQuery({
    queryKey: keys.searchExplain(q),
    queryFn: () => search.explain(q),
    enabled: Boolean(q.trim()),
  })
}

/** Executes bulk operations (mark read, mark unread, delete, move) on entries. */
export function useBulkEntries() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: (payload: BulkEntriesRequest) => entries.bulk(payload),
    onSuccess: () => {
      void client.invalidateQueries({ queryKey: keys.messages() })
      void client.invalidateQueries({ queryKey: keys.mailboxes() })
      void client.invalidateQueries({ queryKey: ['message'] })
      void client.invalidateQueries({ queryKey: ['search'] })
    },
  })
}

/** Direct injection of an RFC 822 / MIME raw message octets. */
export function useInjectMessage() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: (rawBytes: BodyInit) => messages.inject(rawBytes),
    onSuccess: () => {
      void client.invalidateQueries({ queryKey: keys.messages() })
      void client.invalidateQueries({ queryKey: keys.mailboxes() })
    },
  })
}

/** Creates a new mailbox folder. */
export function useCreateMailbox() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: (payload: { name: string; account_id?: string }) =>
      mailboxes.create(payload),
    onSuccess: () => {
      void client.invalidateQueries({ queryKey: keys.mailboxes() })
    },
  })
}

/** Renames a mailbox or toggles subscription. */
export function useRenameMailbox() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: ({ id, name }: { id: string; name: string }) =>
      mailboxes.patch(id, { name }),
    onSuccess: () => {
      void client.invalidateQueries({ queryKey: keys.mailboxes() })
    },
  })
}

/** Deletes a mailbox and all its placements. */
export function useDeleteMailbox() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: (id: string) => mailboxes.delete(id),
    onSuccess: () => {
      void client.invalidateQueries({ queryKey: keys.mailboxes() })
      void client.invalidateQueries({ queryKey: keys.messages() })
    },
  })
}

/** Creates a new user or catch-all account. */
export function useCreateAccount() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: (payload: {
      username: string
      password?: string | undefined
      addresses?: string[] | undefined
      is_catch_all?: boolean | undefined
    }) => accounts.create(payload),
    onSuccess: () => {
      void client.invalidateQueries({ queryKey: keys.accounts() })
      void client.invalidateQueries({ queryKey: keys.mailboxes() })
    },
  })
}

/** Updates an account's credentials, identities, or catch-all status. */
export function usePatchAccount() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: ({
      id,
      ...payload
    }: {
      id: string
      password?: string | undefined
      add_addresses?: string[] | undefined
      remove_addresses?: string[] | undefined
      is_catch_all?: boolean | undefined
    }) => accounts.patch(id, payload),
    onSuccess: () => {
      void client.invalidateQueries({ queryKey: keys.accounts() })
      void client.invalidateQueries({ queryKey: keys.mailboxes() })
    },
  })
}

/** Deletes an account and all mailboxes/messages it owns. */
export function useDeleteAccount() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: (id: string) => accounts.delete(id),
    onSuccess: () => {
      void client.invalidateQueries({ queryKey: keys.accounts() })
      void client.invalidateQueries({ queryKey: keys.mailboxes() })
      void client.invalidateQueries({ queryKey: keys.messages() })
    },
  })
}

/** Lists all workspaces with live message counts and database sizes. */
export function useWorkspaces() {
  return useQuery({
    queryKey: keys.workspaces(),
    queryFn: workspaces.list,
  })
}

/** Fetches a single workspace by slug. */
export function useWorkspace(id: string) {
  return useQuery({
    queryKey: keys.workspace(id),
    queryFn: () => workspaces.get(id),
    enabled: Boolean(id),
  })
}

/** Creates a new workspace. If `set_active` is true, all queries are invalidated. */
export function useCreateWorkspace() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: (payload: Parameters<typeof workspaces.create>[0]) => workspaces.create(payload),
    onSuccess: (data) => {
      void client.invalidateQueries({ queryKey: keys.workspaces() })
      if (data.is_active) {
        // Full invalidation because switching active workspace switches all accounts & mail
        void client.invalidateQueries()
      }
    },
  })
}

/** Switches the active workspace. Invalidates all queries to reflect the new database. */
export function useActivateWorkspace() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: (id: string) => workspaces.activate(id),
    onSuccess: () => {
      // Invalidate everything so inboxes, accounts, traces, metrics immediately switch
      void client.invalidateQueries()
    },
  })
}

/** Purges messages, traces, or performs factory reset on a workspace. */
export function usePurgeWorkspace() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: ({
      id,
      ...options
    }: {
      id: string
      mode?: 'mail_and_traces' | 'factory_reset'
      clear_traces?: boolean
      clear_webhooks?: boolean
    }) => workspaces.purge(id, options),
    onSuccess: () => {
      void client.invalidateQueries({ queryKey: keys.workspaces() })
      void client.invalidateQueries({ queryKey: keys.messages() })
      void client.invalidateQueries({ queryKey: keys.mailboxes() })
      void client.invalidateQueries({ queryKey: keys.accounts() })
      void client.invalidateQueries({ queryKey: keys.connections() })
      void client.invalidateQueries({ queryKey: keys.webhooks() })
      void client.invalidateQueries({ queryKey: keys.outbox() })
    },
  })
}

/** Updates a workspace (name, description, auth requirement). */
export function useUpdateWorkspace() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: ({
      id,
      ...payload
    }: {
      id: string
      name?: string | undefined
      description?: string | null | undefined
      auth_required?: boolean | undefined
    }) => workspaces.update(id, payload),
    onSuccess: (data) => {
      void client.invalidateQueries({ queryKey: keys.workspaces() })
      void client.invalidateQueries({ queryKey: keys.workspace(data.id) })
    },
  })
}

/** Deletes a workspace and removes its database file. */
export function useDeleteWorkspace() {
  const client = useQueryClient()
  return useMutation({
    mutationFn: (id: string) => workspaces.delete(id),
    onSuccess: () => {
      void client.invalidateQueries({ queryKey: keys.workspaces() })
    },
  })
}

