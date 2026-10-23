/**
 * Accessibility gate: axe-core over every principal view.
 *
 * This closes the Phase 4 acceptance criterion "axe-core reports no violations
 * on any principal view", which had no suite behind it.
 *
 * Each view is rendered through the same providers the app uses and checked in
 * its *loaded* state, not its skeleton: an empty view has almost no DOM and
 * would pass trivially, asserting nothing. `src/test/axe.ts` documents which
 * rules jsdom can answer honestly.
 */

import { describe, expect, it, vi, beforeEach } from 'vitest'
import { waitFor } from '@testing-library/react'

import { renderWithProviders } from './render'
import { checkA11y } from './axe'

import { OverviewRoute } from '../routes/OverviewRoute'
import { WorkspacesRoute } from '../routes/WorkspacesRoute'
import { SearchRoute } from '../routes/SearchRoute'
import { WebhooksRoute } from '../routes/WebhooksRoute'
import { LogsRoute } from '../routes/LogsRoute'
import { TracesRoute } from '../routes/TracesRoute'
import { AboutRoute } from '../routes/AboutRoute'

// Resources are mocked wholesale so a view's accessibility is asserted against
// realistic content rather than an error state, which renders a different tree.
vi.mock('../api/resources', () => {
  const account = {
    id: 'acc-1',
    username: 'dev',
    is_catch_all: true,
    created_at: '2026-01-01T00:00:00Z',
    addresses: ['dev@example.test'],
  }

  const mailbox = {
    id: 'mbx-1',
    account_id: 'acc-1',
    name: 'INBOX',
    parent_id: null,
    uid_validity: 1,
    uid_next: 2,
    subscribed: true,
    attributes: [],
    message_count: 1,
    unseen_count: 1,
    created_at: '2026-01-01T00:00:00Z',
  }

  const workspace = {
    id: 'default',
    name: 'Default Workspace',
    description: 'Local development workspace',
    is_active: true,
    message_count: 1,
    size_bytes: 2048,
    database_path: '/tmp/sandesh/default.db',
    created_at: '2026-01-01T00:00:00Z',
  }

  return {
    PAGE_SIZE: 50,
    health: {
      get: vi.fn().mockResolvedValue({
        status: 'ok',
        version: '0.1.0',
        uptime_seconds: 120,
        listeners: [{ protocol: 'smtp', address: '127.0.0.1:1025', tls: false }],
      }),
    },
    accounts: {
      list: vi.fn().mockResolvedValue({ items: [account] }),
      get: vi.fn().mockResolvedValue(account),
    },
    mailboxes: {
      list: vi.fn().mockResolvedValue({ items: [mailbox] }),
      get: vi.fn().mockResolvedValue(mailbox),
    },
    messages: {
      list: vi.fn().mockResolvedValue({ items: [], next_cursor: null }),
      get: vi.fn().mockResolvedValue(null),
    },
    entries: {
      list: vi.fn().mockResolvedValue({ items: [], next_cursor: null }),
    },
    search: {
      query: vi.fn().mockResolvedValue({ items: [], total: 0, took_ms: 1 }),
    },
    webhooks: {
      list: vi.fn().mockResolvedValue([
        {
          id: 'wh-1',
          url: 'https://example.test/hook',
          events: ['message.received'],
          enabled: true,
          created_at: '2026-01-01T00:00:00Z',
        },
      ]),
      create: vi.fn(),
      delete: vi.fn(),
      update: vi.fn(),
    },
    logs: {
      list: vi.fn().mockResolvedValue({
        items: [
          {
            seq: 1,
            timestamp: '2026-01-01T00:00:00Z',
            level: 'INFO',
            target: 'sandesh_smtp',
            message: 'listening on 127.0.0.1:1025',
          },
        ],
      }),
      streamUrl: vi.fn().mockReturnValue('http://localhost/api/v1/logs/stream'),
    },
    connections: {
      list: vi.fn().mockResolvedValue({
        items: [
          {
            id: 'conn-1',
            protocol: 'smtp',
            peer: '127.0.0.1:54321',
            opened_at: '2026-01-01T00:00:00Z',
            closed_at: null,
            transcript_lines: 4,
          },
        ],
      }),
      get: vi.fn().mockResolvedValue(null),
    },
    faults: {
      list: vi.fn().mockResolvedValue([]),
      set: vi.fn(),
      clear: vi.fn(),
    },
    transfer: { export: vi.fn(), import: vi.fn() },
    attachments: {},
    workspaces: {
      list: vi.fn().mockResolvedValue([workspace]),
      activate: vi.fn(),
      create: vi.fn(),
      purge: vi.fn(),
      delete: vi.fn(),
    },
  }
})

/**
 * Each principal view, with a marker proving it reached its loaded state.
 *
 * The marker matters: axe over a spinner is a test that cannot fail.
 */
const VIEWS: ReadonlyArray<{
  name: string
  element: React.ReactElement
  settled: RegExp
}> = [
  {
    name: 'Overview',
    element: <OverviewRoute />,
    settled: /overview|status|listener/i,
  },
  { name: 'Workspaces', element: <WorkspacesRoute />, settled: /workspace/i },
  { name: 'Search', element: <SearchRoute />, settled: /search/i },
  { name: 'Webhooks', element: <WebhooksRoute />, settled: /webhook/i },
  { name: 'Logs', element: <LogsRoute />, settled: /log/i },
  { name: 'Traces', element: <TracesRoute />, settled: /trace|connection/i },
  { name: 'About', element: <AboutRoute />, settled: /about|sandesh/i },
]

describe('accessibility', () => {
  beforeEach(() => {
    vi.clearAllMocks()
  })

  it.each(VIEWS)('$name has no axe violations', async ({ element, settled }) => {
    const { container } = renderWithProviders(element)

    // Wait for real content before asserting, so the check has a tree to walk.
    await waitFor(() => {
      expect(container.textContent).toMatch(settled)
    })

    expect(await checkA11y(container)).toHaveNoViolations()
  })
})
