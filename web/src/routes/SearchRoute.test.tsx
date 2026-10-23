import { beforeEach, describe, expect, it, vi } from 'vitest'
import { fireEvent, screen, waitFor } from '@testing-library/react'
import { Route, Routes } from 'react-router-dom'

import { renderWithProviders } from '../test/render'
import { SearchRoute } from './SearchRoute'
import {
  entries as entriesApi,
  messages as messagesApi,
  search as searchApi,
} from '../api/resources'

vi.mock('../api/resources', () => ({
  mailboxes: {
    list: vi.fn().mockResolvedValue({ items: [] }),
  },
  messages: {
    list: vi.fn().mockResolvedValue({ items: [] }),
    get: vi.fn(),
    headers: vi.fn().mockResolvedValue([]),
    mime: vi.fn().mockResolvedValue({ html_parts: [], text_parts: [] }),
    raw: vi.fn().mockResolvedValue(''),
  },
  search: {
    messages: vi.fn(),
    explain: vi.fn().mockResolvedValue({ plan: 'FTS_MATCH("urgent")' }),
  },
  entries: {
    setFlags: vi.fn(),
    bulk: vi.fn(),
    move: vi.fn(),
    remove: vi.fn(),
  },
  accounts: {
    list: vi.fn().mockResolvedValue({ items: [] }),
  },
}))

const sampleSearchResult1 = {
  id: 'search-msg-1',
  account_id: 'acc-1',
  subject: 'Urgent Security Alert',
  snippet: 'Suspicious login detected',
  date: '2026-09-10T10:00:00Z',
  received_at: '2026-09-10T10:00:00Z',
  parse_ok: true,
  size: 2048,
  has_attachments: false,
  diagnostics: [],
  from: { display_name: 'Security Ops', address: 'sec@example.test' },
  to: [{ display_name: 'Me', address: 'me@example.test' }],
  entries: [
    {
      entry_id: 'entry-sec-1',
      mailbox_id: 'mb-inbox',
      flags: [], // unread
      internal_date: '2026-09-10T10:00:00Z',
      uid: 1,
    },
  ],
}

const sampleSearchResult2 = {
  id: 'search-msg-2',
  account_id: 'acc-1',
  subject: 'Weekly Digest',
  snippet: 'Here is your weekly roundup',
  date: '2026-09-09T12:00:00Z',
  received_at: '2026-09-09T12:00:00Z',
  parse_ok: true,
  size: 4096,
  has_attachments: true,
  diagnostics: [],
  from: { display_name: 'Newsletter', address: 'news@example.test' },
  to: [{ display_name: 'Me', address: 'me@example.test' }],
  entries: [
    {
      entry_id: 'entry-sec-2',
      mailbox_id: 'mb-inbox',
      flags: ['\\Seen'], // read
      internal_date: '2026-09-09T12:00:00Z',
      uid: 2,
    },
  ],
}

function renderSearch(initialRoute = '/search') {
  return renderWithProviders(
    <Routes>
      <Route path="search" element={<SearchRoute />} />
      <Route path="search/message/:messageId" element={<SearchRoute />} />
    </Routes>,
    { route: initialRoute },
  )
}

describe('SearchRoute', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    vi.mocked(searchApi.messages).mockResolvedValue({
      items: [sampleSearchResult1, sampleSearchResult2],
      next_cursor: null,
      total_estimate: 2,
    })
    vi.mocked(searchApi.explain).mockResolvedValue({
      query: 'urgent',
      plan: 'SCAN entries WHERE flags NOT MATCH \\Seen',
    })
    vi.mocked(messagesApi.get).mockImplementation(async (id: string) => {
      const found = [sampleSearchResult1, sampleSearchResult2].find((m) => m.id === id)
      if (!found) throw new Error('Not found')
      return {
        ...found,
        html_body: '<p>Search result content</p>',
        text_body: 'Search result content',
        attachments: [],
        diagnostics: [],
        headers: [],
        source: '',
      }
    })
    vi.mocked(entriesApi.setFlags).mockResolvedValue({
      id: 'entry-sec-1',
      mailbox_id: 'mb-inbox',
      message_id: 'search-msg-1',
      uid: 1,
      flags: ['\\Seen'],
      internal_date: '2026-09-10T10:00:00Z',
    })
    vi.mocked(entriesApi.bulk).mockResolvedValue({ affected: 1 })
  })

  it('renders search guide chips when no query is present', () => {
    renderSearch('/search')

    expect(screen.getByRole('heading', { name: 'Search' })).toBeInTheDocument()
    expect(screen.getByText('Filter Syntax')).toBeInTheDocument()
    expect(screen.getByRole('searchbox', { name: 'Search messages' })).toBeInTheDocument()
  })

  it('renders search results and explain plan when query is present', async () => {
    renderSearch('/search?q=urgent')

    await waitFor(() => {
      expect(searchApi.messages).toHaveBeenCalledWith({ q: 'urgent', mailboxId: undefined })
      expect(screen.getByText('Urgent Security Alert')).toBeInTheDocument()
      expect(screen.getByText('Weekly Digest')).toBeInTheDocument()
      expect(screen.getByText(/Plan: SCAN entries/)).toBeInTheDocument()
    })
  })

  it('marks unread search result as read on click and navigates to message', async () => {
    renderSearch('/search?q=urgent')

    await waitFor(() => {
      expect(screen.getByText('Urgent Security Alert')).toBeInTheDocument()
    })

    const unreadItem = screen.getByText('Urgent Security Alert')
    fireEvent.click(unreadItem)

    await waitFor(() => {
      expect(entriesApi.setFlags).toHaveBeenCalledWith('entry-sec-1', { add: ['\\Seen'] })
    })
  })

  it('toggles quick filter pills to compose search queries', async () => {
    renderSearch('/search')

    const toolbar = screen.getByRole('toolbar', { name: 'Search quick filters' })
    const unreadPill = toolbar.querySelector('.search-pill')!
    fireEvent.click(unreadPill)

    expect(unreadPill).toHaveAttribute('aria-pressed', 'true')
    const searchInput = screen.getByRole('searchbox', { name: 'Search messages' }) as HTMLInputElement
    expect(searchInput.value).toContain('is:unread')
  })

  it('appends prefix filter pill to search input', async () => {
    renderSearch('/search')

    const fromPill = screen.getByRole('button', { name: 'from:' })
    fireEvent.click(fromPill)

    const searchInput = screen.getByRole('searchbox', { name: 'Search messages' }) as HTMLInputElement
    expect(searchInput.value).toBe('from:')
  })

  it('clears query when clicking clear search input button', async () => {
    renderSearch('/search?q=something')

    const clearBtn = screen.getByLabelText('Clear search input')
    fireEvent.click(clearBtn)

    const searchInput = screen.getByRole('searchbox', { name: 'Search messages' }) as HTMLInputElement
    expect(searchInput.value).toBe('')
  })

  it('supports bulk actions on search results', async () => {
    renderSearch('/search?q=test')

    await waitFor(() => {
      expect(screen.getByText('Urgent Security Alert')).toBeInTheDocument()
    })

    // Multi-select with metaKey
    const unreadItem = screen.getByText('Urgent Security Alert')
    fireEvent.click(unreadItem, { metaKey: true })

    await waitFor(() => {
      expect(screen.getByRole('toolbar', { name: /Bulk actions/i })).toBeInTheDocument()
      expect(screen.getByText('1 selected')).toBeInTheDocument()
    })

    const bulkReadBtn = screen.getByRole('button', { name: /Mark selected as read/i })
    fireEvent.click(bulkReadBtn)

    await waitFor(() => {
      expect(entriesApi.bulk).toHaveBeenCalledWith({
        entry_ids: ['entry-sec-1'],
        action: 'mark_read',
      })
    })
  })
})
