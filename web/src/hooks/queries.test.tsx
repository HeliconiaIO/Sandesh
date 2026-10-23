/**
 * Cache invalidation after a mutation (§65).
 *
 * The bug this guards against is subtle: message *lists* live under
 * `['messages']` while an open message lives under `['message', id]`, so
 * invalidating the former leaves the viewer showing the flags it loaded with.
 * The symptom is a "Mark read" button that does nothing visible even though
 * the server accepted the change.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderHook, waitFor } from '@testing-library/react'
import type { ReactNode } from 'react'

import { useSetFlags, useDeleteEntry } from './queries'
import * as resources from '../api/resources'

let client: QueryClient

function wrapper({ children }: { children: ReactNode }) {
  return <QueryClientProvider client={client}>{children}</QueryClientProvider>
}

/** The query keys an invalidation call touched. */
function invalidatedKeys(calls: unknown[][]): string[] {
  return calls.map((call) => JSON.stringify((call[0] as { queryKey?: unknown } | undefined)?.queryKey))
}

beforeEach(() => {
  client = new QueryClient({ defaultOptions: { queries: { retry: false } } })
})

afterEach(() => {
  vi.restoreAllMocks()
})

describe('useSetFlags', () => {
  it('refreshes the open message as well as the lists', async () => {
    vi.spyOn(resources.entries, 'setFlags').mockResolvedValue({
      id: 'e1',
      message_id: 'm1',
      mailbox_id: 'b1',
      uid: 1,
      flags: ['\\Seen'],
      internal_date: new Date().toISOString(),
    })
    const invalidate = vi.spyOn(client, 'invalidateQueries')

    const { result } = renderHook(() => useSetFlags(), { wrapper })
    result.current.mutate({ entryId: 'e1', add: ['\\Seen'] })

    await waitFor(() => expect(result.current.isSuccess).toBe(true))

    const keys = invalidatedKeys(invalidate.mock.calls)
    expect(keys).toContain(JSON.stringify(['messages']))
    expect(keys).toContain(JSON.stringify(['mailboxes']))
    // The one that was missing: without it the viewer shows stale flags.
    expect(keys).toContain(JSON.stringify(['message']))
  })
})

describe('useDeleteEntry', () => {
  it('refreshes the lists and the counts after a removal', async () => {
    vi.spyOn(resources.entries, 'remove').mockResolvedValue(undefined)
    const invalidate = vi.spyOn(client, 'invalidateQueries')

    const { result } = renderHook(() => useDeleteEntry(), { wrapper })
    result.current.mutate('e1')

    await waitFor(() => expect(result.current.isSuccess).toBe(true))

    const keys = invalidatedKeys(invalidate.mock.calls)
    expect(keys).toContain(JSON.stringify(['messages']))
    expect(keys).toContain(JSON.stringify(['mailboxes']))
  })
})
