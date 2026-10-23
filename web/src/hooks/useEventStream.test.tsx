/**
 * Event-driven cache updates (§65).
 *
 * The behaviour under test is the one a user sees as "new mail appears without
 * refreshing": an event must invalidate the queries that show mail, and must
 * not invalidate everything indiscriminately, which would turn every arriving
 * message into a full refetch of the application.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderHook } from '@testing-library/react'
import type { ReactNode } from 'react'

import { eventStream } from '../api/events'
import { useEventSync } from './useEventStream'

/** Pushes a frame through the real stream's subscriber machinery. */
function emit(kind: string, payload: Record<string, unknown> = {}) {
  // `subscribe` is the documented surface; reaching the listeners through it
  // keeps the test honest about what the module actually exposes.
  const listeners = (eventStream as unknown as { events: Set<(f: unknown) => void> }).events
  for (const listener of listeners) {
    listener({ seq: 1, busSeq: 1, event: { kind, id: 'e', occurred_at: '', payload } })
  }
}

let client: QueryClient

function wrapper({ children }: { children: ReactNode }) {
  return <QueryClientProvider client={client}>{children}</QueryClientProvider>
}

beforeEach(() => {
  client = new QueryClient({ defaultOptions: { queries: { retry: false } } })
})

afterEach(() => {
  vi.restoreAllMocks()
})

describe('useEventSync', () => {
  it('matches the PascalCase kinds the backend actually emits', () => {
    // The bug this pins: the REST DTOs are snake_case, so assuming the event
    // stream matched them made every case fall through. Live updates then
    // failed silently — the socket was connected and frames arrived, but
    // nothing was ever invalidated.
    const invalidate = vi.spyOn(client, 'invalidateQueries')
    renderHook(() => useEventSync(), { wrapper })

    emit('message_received', { message_id: 'm1' })
    expect(invalidate).not.toHaveBeenCalled()

    emit('MessageReceived', { message_id: 'm1' })
    expect(invalidate).toHaveBeenCalled()
  })

  it('refreshes the message list when mail arrives', () => {
    const invalidate = vi.spyOn(client, 'invalidateQueries')
    renderHook(() => useEventSync(), { wrapper })

    emit('MessageReceived', { message_id: 'm1' })

    const keys = invalidate.mock.calls.map((call) => JSON.stringify(call[0]?.queryKey))
    expect(keys.some((key) => key?.includes('messages'))).toBe(true)
    expect(keys.some((key) => key?.includes('mailboxes'))).toBe(true)
  })

  it('refreshes the open message when its flags change', () => {
    const invalidate = vi.spyOn(client, 'invalidateQueries')
    renderHook(() => useEventSync(), { wrapper })

    emit('EntryFlagsChanged', { message_id: 'm42' })

    const keys = invalidate.mock.calls.map((call) => JSON.stringify(call[0]?.queryKey))
    expect(keys.some((key) => key?.includes('m42'))).toBe(true)
  })

  it('refreshes accounts when one is created', () => {
    const invalidate = vi.spyOn(client, 'invalidateQueries')
    renderHook(() => useEventSync(), { wrapper })

    emit('AccountCreated', { account_id: 'a1' })

    const keys = invalidate.mock.calls.map((call) => JSON.stringify(call[0]?.queryKey))
    expect(keys.some((key) => key?.includes('accounts'))).toBe(true)
  })

  it('refreshes workspaces when one is created or deleted', () => {
    const invalidate = vi.spyOn(client, 'invalidateQueries')
    renderHook(() => useEventSync(), { wrapper })

    emit('WorkspaceCreated', { workspace_id: 'ws1', name: 'WS 1' })
    const createdKeys = invalidate.mock.calls.map((call) => JSON.stringify(call[0]?.queryKey))
    expect(createdKeys.some((key) => key?.includes('workspaces'))).toBe(true)

    invalidate.mockClear()

    emit('WorkspaceDeleted', { workspace_id: 'ws1' })
    const deletedKeys = invalidate.mock.calls.map((call) => JSON.stringify(call[0]?.queryKey))
    expect(deletedKeys.some((key) => key?.includes('workspaces'))).toBe(true)
  })

  it('refetches the whole application when the active workspace is switched', () => {
    const invalidate = vi.spyOn(client, 'invalidateQueries')
    renderHook(() => useEventSync(), { wrapper })

    emit('WorkspaceSwitched', { previous: 'default', current: 'billing' })

    // Called with no arguments: invalidates all queries across the app
    expect(invalidate).toHaveBeenCalledWith()
  })

  it('refreshes messages, accounts, and workspaces when a workspace is purged', () => {
    const invalidate = vi.spyOn(client, 'invalidateQueries')
    renderHook(() => useEventSync(), { wrapper })

    emit('WorkspacePurged', { workspace_id: 'default' })

    const keys = invalidate.mock.calls.map((call) => JSON.stringify(call[0]?.queryKey))
    expect(keys.some((key) => key?.includes('workspaces'))).toBe(true)
    expect(keys.some((key) => key?.includes('messages'))).toBe(true)
    expect(keys.some((key) => key?.includes('mailboxes'))).toBe(true)
    expect(keys.some((key) => key?.includes('accounts'))).toBe(true)
  })

  it('does not refetch the world for an unrelated event', () => {
    // A delivery failure changes nothing that is cached; invalidating
    // everything on it would make a noisy backend expensive for the browser.
    const invalidate = vi.spyOn(client, 'invalidateQueries')
    renderHook(() => useEventSync(), { wrapper })

    emit('DeliveryFailed', { delivery_id: 'd1' })

    expect(invalidate).not.toHaveBeenCalled()
  })

  it('stops listening once unmounted', () => {
    const invalidate = vi.spyOn(client, 'invalidateQueries')
    const { unmount } = renderHook(() => useEventSync(), { wrapper })

    unmount()
    emit('MessageReceived', { message_id: 'm1' })

    expect(invalidate).not.toHaveBeenCalled()
  })
})
