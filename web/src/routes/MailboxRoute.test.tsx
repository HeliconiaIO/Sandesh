import { beforeEach, describe, expect, it, vi } from 'vitest'
import { fireEvent, screen, waitFor } from '@testing-library/react'
import { Route, Routes } from 'react-router-dom'

import { renderWithProviders } from '../test/render'
import { MailboxRoute } from './MailboxRoute'
import {
  entries as entriesApi,
  mailboxes as mailboxesApi,
  messages as messagesApi,
} from '../api/resources'

vi.mock('../api/resources', () => ({
  mailboxes: {
    list: vi.fn(),
  },
  messages: {
    list: vi.fn(),
    get: vi.fn(),
    headers: vi.fn().mockResolvedValue([]),
    mime: vi.fn().mockResolvedValue({ html_parts: [], text_parts: [] }),
    raw: vi.fn().mockResolvedValue(''),
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

const sampleMailboxes = {
  items: [
    {
      id: 'mb-1',
      account_id: 'acc-1',
      name: 'INBOX',
      subscribed: true,
      total: 2,
      unseen: 1,
      uid_validity: 1,
      uid_next: 3,
    },
    {
      id: 'mb-archive',
      account_id: 'acc-1',
      name: 'Archive',
      subscribed: true,
      total: 5,
      unseen: 0,
      uid_validity: 1,
      uid_next: 6,
    },
  ],
  next_cursor: null,
  total_estimate: 2,
}

const sampleMsg1 = {
  id: 'msg-1',
  account_id: 'acc-1',
  subject: 'Unread Urgent Notice',
  snippet: 'Please read this immediately',
  date: '2026-09-10T10:00:00Z',
  received_at: '2026-09-10T10:00:00Z',
  parse_ok: true,
  size: 1024,
  has_attachments: false,
  diagnostics: [],
  from: { display_name: 'Alerts', address: 'alerts@example.test' },
  to: [{ display_name: 'Me', address: 'me@example.test' }],
  entries: [
    {
      entry_id: 'entry-1',
      mailbox_id: 'mb-1',
      flags: [], // unread
      internal_date: '2026-09-10T10:00:00Z',
      uid: 1,
    },
  ],
}

const sampleMsg2 = {
  id: 'msg-2',
  account_id: 'acc-1',
  subject: 'Monthly Report with PDF',
  snippet: 'Here is your monthly invoice',
  date: '2026-09-09T08:00:00Z',
  received_at: '2026-09-09T08:00:00Z',
  parse_ok: true,
  size: 5120,
  has_attachments: true,
  diagnostics: [],
  from: { display_name: 'Finance Team', address: 'finance@example.test' },
  to: [{ display_name: 'Me', address: 'me@example.test' }],
  entries: [
    {
      entry_id: 'entry-2',
      mailbox_id: 'mb-1',
      flags: ['\\Seen'], // read
      internal_date: '2026-09-09T08:00:00Z',
      uid: 2,
    },
  ],
}

function renderMailbox(initialRoute = '/mailbox/mb-1') {
  return renderWithProviders(
    <Routes>
      <Route path="mailbox/:mailboxId" element={<MailboxRoute />} />
      <Route path="mailbox/:mailboxId/message/:messageId" element={<MailboxRoute />} />
    </Routes>,
    { route: initialRoute },
  )
}

describe('MailboxRoute', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    vi.mocked(mailboxesApi.list).mockResolvedValue(sampleMailboxes)
    vi.mocked(messagesApi.list).mockResolvedValue({
      items: [sampleMsg1, sampleMsg2],
      next_cursor: null,
      total_estimate: 2,
    })
    vi.mocked(messagesApi.get).mockImplementation(async (id: string) => {
      const found = [sampleMsg1, sampleMsg2].find((m) => m.id === id)
      if (!found) throw new Error('Not found')
      return {
        ...found,
        html_body: '<p>Message body</p>',
        text_body: 'Message body',
        attachments: [],
        diagnostics: [],
        headers: [],
        source: '',
      }
    })
    vi.mocked(entriesApi.setFlags).mockResolvedValue({
      id: 'entry-1',
      mailbox_id: 'mb-1',
      message_id: 'msg-1',
      uid: 1,
      flags: ['\\Seen'],
      internal_date: '2026-09-10T10:00:00Z',
    })
    vi.mocked(entriesApi.bulk).mockResolvedValue({ affected: 1 })
  })

  it('renders mailbox messages list', async () => {
    renderMailbox()

    await waitFor(() => {
      expect(screen.getByText('INBOX')).toBeInTheDocument()
      expect(screen.getByText('Unread Urgent Notice')).toBeInTheDocument()
      expect(screen.getByText('Monthly Report with PDF')).toBeInTheDocument()
    })
  })

  it('marks unread email as read on click and navigates to message', async () => {
    renderMailbox()

    await waitFor(() => {
      expect(screen.getByText('Unread Urgent Notice')).toBeInTheDocument()
    })

    const unreadItem = screen.getByText('Unread Urgent Notice')
    fireEvent.click(unreadItem)

    // Should call setFlags with { add: ['\\Seen'] }
    await waitFor(() => {
      expect(entriesApi.setFlags).toHaveBeenCalledWith('entry-1', { add: ['\\Seen'] })
    })
  })

  it('does not call setFlags if already read when clicked', async () => {
    renderMailbox()

    await waitFor(() => {
      expect(screen.getByText('Monthly Report with PDF')).toBeInTheDocument()
    })

    const readItem = screen.getByText('Monthly Report with PDF')
    fireEvent.click(readItem)

    expect(entriesApi.setFlags).not.toHaveBeenCalled()
  })

  it('filters messages by text input', async () => {
    renderMailbox()

    await waitFor(() => {
      expect(screen.getByText('Unread Urgent Notice')).toBeInTheDocument()
    })

    const filterInput = screen.getByPlaceholderText(/Filter sender or subject/i)
    fireEvent.change(filterInput, { target: { value: 'Finance' } })

    expect(screen.queryByText('Unread Urgent Notice')).not.toBeInTheDocument()
    expect(screen.getByText('Monthly Report with PDF')).toBeInTheDocument()
  })

  it('toggles is:unread filter pill', async () => {
    renderMailbox()

    await waitFor(() => {
      expect(screen.getByText('Unread Urgent Notice')).toBeInTheDocument()
    })

    const unreadPill = screen.getByRole('button', { name: 'is:unread' })
    fireEvent.click(unreadPill)

    expect(unreadPill).toHaveAttribute('aria-pressed', 'true')
    expect(screen.getByText('Unread Urgent Notice')).toBeInTheDocument()
    expect(screen.queryByText('Monthly Report with PDF')).not.toBeInTheDocument()

    // Toggle off
    fireEvent.click(unreadPill)
    expect(unreadPill).toHaveAttribute('aria-pressed', 'false')
    expect(screen.getByText('Monthly Report with PDF')).toBeInTheDocument()
  })

  it('toggles has:attachment filter pill', async () => {
    renderMailbox()

    await waitFor(() => {
      expect(screen.getByText('Monthly Report with PDF')).toBeInTheDocument()
    })

    const attachmentPill = screen.getByRole('button', { name: 'has:attachment' })
    fireEvent.click(attachmentPill)

    expect(attachmentPill).toHaveAttribute('aria-pressed', 'true')
    expect(screen.queryByText('Unread Urgent Notice')).not.toBeInTheDocument()
    expect(screen.getByText('Monthly Report with PDF')).toBeInTheDocument()
  })


  it('filters messages using from:, to:, and subject: prefixes', async () => {
    renderMailbox()

    await waitFor(() => {
      expect(screen.getByText('Unread Urgent Notice')).toBeInTheDocument()
      expect(screen.getByText('Monthly Report with PDF')).toBeInTheDocument()
    })

    const filterInput = screen.getByPlaceholderText(/Filter sender or subject/i)

    // Filter by from:
    fireEvent.change(filterInput, { target: { value: 'from:Alerts' } })
    expect(screen.getByText('Unread Urgent Notice')).toBeInTheDocument()
    expect(screen.queryByText('Monthly Report with PDF')).not.toBeInTheDocument()

    // Filter by subject:
    fireEvent.change(filterInput, { target: { value: 'subject:Report' } })
    expect(screen.queryByText('Unread Urgent Notice')).not.toBeInTheDocument()
    expect(screen.getByText('Monthly Report with PDF')).toBeInTheDocument()

    // Filter by to:
    fireEvent.change(filterInput, { target: { value: 'to:me@example.test' } })
    expect(screen.getByText('Unread Urgent Notice')).toBeInTheDocument()
    expect(screen.getByText('Monthly Report with PDF')).toBeInTheDocument()
  })

  it('appends prefix when clicking from: or subject: pills', async () => {
    renderMailbox()

    await waitFor(() => {
      expect(screen.getByText('Unread Urgent Notice')).toBeInTheDocument()
    })

    const fromPill = screen.getByRole('button', { name: 'from:' })
    fireEvent.click(fromPill)

    const filterInput = screen.getByPlaceholderText(/Filter sender or subject/i) as HTMLInputElement
    expect(filterInput.value).toBe('from:')
  })

  it('opens filter builder popover and applies structured filter', async () => {
    renderMailbox()

    await waitFor(() => {
      expect(screen.getByText('Unread Urgent Notice')).toBeInTheDocument()
    })

    const optionsBtn = screen.getByRole('button', { name: /Filter options/i })
    fireEvent.click(optionsBtn)

    expect(screen.getByRole('dialog', { name: /Filter options/i })).toBeInTheDocument()

    const fromField = screen.getByPlaceholderText(/e\.g\. alice@example\.com/i)
    fireEvent.change(fromField, { target: { value: 'Alerts' } })

    const applyBtn = screen.getByRole('button', { name: 'Apply Filter' })
    fireEvent.click(applyBtn)

    // Dialog should close and filter input should have from:Alerts
    expect(screen.queryByRole('dialog', { name: /Filter options/i })).not.toBeInTheDocument()
    expect(screen.getByText('Unread Urgent Notice')).toBeInTheDocument()
    expect(screen.queryByText('Monthly Report with PDF')).not.toBeInTheDocument()
  })

  it('supports keyboard navigation (j/k)', async () => {
    renderMailbox('/mailbox/mb-1/message/msg-1')

    await waitFor(() => {
      expect(screen.getAllByText('Unread Urgent Notice')[0]).toBeInTheDocument()
    })

    const listbox = screen.getByRole('listbox', { name: 'Messages' })

    // Press j to go to next message
    fireEvent.keyDown(listbox, { key: 'j' })

    // Press k to go back to previous message
    fireEvent.keyDown(listbox, { key: 'k' })
  })

  it('handles bulk selection and actions', async () => {
    renderMailbox()

    await waitFor(() => {
      expect(screen.getByText('Unread Urgent Notice')).toBeInTheDocument()
    })

    // Select with metaKey / ctrlKey
    const unreadItem = screen.getByText('Unread Urgent Notice')
    fireEvent.click(unreadItem, { metaKey: true })

    // Bulk toolbar should appear
    await waitFor(() => {
      expect(screen.getByRole('toolbar', { name: /Bulk actions/i })).toBeInTheDocument()
      expect(screen.getByText('1 selected')).toBeInTheDocument()
    })

    // Click bulk mark unread
    const bulkUnreadBtn = screen.getByRole('button', { name: /Mark selected as unread/i })
    fireEvent.click(bulkUnreadBtn)

    await waitFor(() => {
      expect(entriesApi.bulk).toHaveBeenCalledWith({
        entry_ids: ['entry-1'],
        action: 'mark_unread',
      })
    })
  })
})
