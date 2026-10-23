import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { ReactNode } from 'react'

import { MessageViewer } from './MessageViewer'
import { ComposerProvider } from '../composer/ComposerContext'
import { DialogProvider } from '../../context/DialogContext'
import { detail, mimeTree } from '../../test/fixtures'
import * as resources from '../../api/resources'

let client: QueryClient

function wrapper({ children }: { children: ReactNode }) {
  return (
    <QueryClientProvider client={client}>
      <DialogProvider>
        <ComposerProvider>{children}</ComposerProvider>
      </DialogProvider>
    </QueryClientProvider>
  )
}

beforeEach(() => {
  client = new QueryClient({
    defaultOptions: {
      queries: { retry: false },
    },
  })
  vi.spyOn(resources.messages, 'headers').mockResolvedValue([])
  vi.spyOn(resources.messages, 'mime').mockResolvedValue(
    mimeTree({
      html_parts: ['1'],
      text_parts: ['0'],
    }),
  )
  vi.spyOn(resources.messages, 'raw').mockResolvedValue('raw message bytes')
  vi.spyOn(resources.messages, 'part').mockResolvedValue('<h1>Welcome to Sandesh</h1>')
  vi.spyOn(resources.mailboxes, 'list').mockResolvedValue({
    items: [],
    next_cursor: null,
    total_estimate: 0,
  })
  vi.spyOn(resources.entries, 'setFlags').mockResolvedValue({
    id: '01920000-0000-7000-8000-0000000000e1',
    mailbox_id: '01920000-0000-7000-8000-0000000000b1',
    message_id: '01920000-0000-7000-8000-000000000001',
    uid: 1,
    internal_date: '2026-01-01T00:00:00Z',
    flags: ['\\Seen'],
  })
})

afterEach(() => {
  vi.restoreAllMocks()
})

describe('MessageViewer Viewport & Virtual Device', () => {
  it('toggles between Desktop, Mobile, and Tablet virtual devices', async () => {
    render(
      <MessageViewer
        message={detail({ id: 'msg-1', parse_ok: true })}
        mailboxId="b1"
        onClose={() => {}}
      />,
      { wrapper },
    )

    // Wait for the preview content to load
    await waitFor(() => {
      expect(screen.getByTitle('Message body')).toBeInTheDocument()
    })

    // Initially in Desktop mode
    expect(screen.getByRole('button', { name: 'Desktop view' })).toHaveClass('viewport-btn--active')
    expect(screen.queryByLabelText(/Mobile preview/i)).not.toBeInTheDocument()

    // Switch to Mobile
    const mobileBtn = screen.getByRole('button', { name: 'Mobile view' })
    fireEvent.click(mobileBtn)

    expect(mobileBtn).toHaveClass('viewport-btn--active')
    const mobileDevice = screen.getByLabelText(/Mobile preview \(portrait\)/i)
    expect(mobileDevice).toBeInTheDocument()
    expect(screen.getByText('390 × 800')).toBeInTheDocument()
    expect(screen.getByText('9:41')).toBeInTheDocument()

    // Rotate Mobile to Landscape
    const rotateBtn = screen.getByRole('button', { name: /Switch orientation/i })
    fireEvent.click(rotateBtn)

    expect(screen.getByLabelText(/Mobile preview \(landscape\)/i)).toBeInTheDocument()
    expect(screen.getByText('800 × 390')).toBeInTheDocument()

    // Switch to Tablet
    const tabletBtn = screen.getByRole('button', { name: 'Tablet view' })
    fireEvent.click(tabletBtn)

    expect(tabletBtn).toHaveClass('viewport-btn--active')
    const tabletDevice = screen.getByLabelText(/Tablet preview/i)
    expect(tabletDevice).toBeInTheDocument()

    // Switch back to Desktop
    const desktopBtn = screen.getByRole('button', { name: 'Desktop view' })
    fireEvent.click(desktopBtn)

    expect(desktopBtn).toHaveClass('viewport-btn--active')
    expect(screen.queryByLabelText(/Tablet preview/i)).not.toBeInTheDocument()
    expect(screen.queryByLabelText(/Mobile preview/i)).not.toBeInTheDocument()
  })

  it('loads HTML draft body and activates HTML mode when clicking Edit Draft', async () => {
    vi.spyOn(resources.messages, 'mime').mockResolvedValue(
      mimeTree({
        html_parts: ['2'],
        text_parts: [],
      }),
    )
    vi.spyOn(resources.messages, 'part').mockResolvedValue('<p>Actual draft text</p>')
    vi.spyOn(resources.mailboxes, 'list').mockResolvedValue({
      items: [
        {
          id: 'drafts-mbx',
          name: 'Drafts',
          account_id: 'acc-1',
          uid_validity: 1,
          uid_next: 2,
          total: 1,
          unseen: 0,
          subscribed: true,
        },
      ],
      next_cursor: null,
      total_estimate: 1,
    })

    render(
      <MessageViewer
        message={detail({
          id: 'msg-draft-1',
          subject: 'Draft Subject',
          from: { address: 'admin@example.com', display_name: null },
          to: [{ address: 'recipient@example.com', display_name: null }],
        })}
        mailboxId="drafts-mbx"
        onClose={() => {}}
      />,
      { wrapper },
    )

    await waitFor(() => {
      expect(screen.getByRole('button', { name: /Edit Draft/i })).toBeInTheDocument()
    })

    const editBtn = screen.getByRole('button', { name: /Edit Draft/i })
    fireEvent.click(editBtn)

    // Verify composer opened with HTML tab active and actual draft text rendered
    await waitFor(() => {
      expect(screen.getByRole('dialog')).toBeInTheDocument()
      expect(screen.getByRole('button', { name: 'HTML' })).toHaveClass('composer-segmented-btn--active')
      expect(screen.getByText('Actual draft text')).toBeInTheDocument()
    })
  })

  it('toggles read and unread flag when clicking the toggle button', async () => {
    const setFlagsSpy = vi.spyOn(resources.entries, 'setFlags').mockResolvedValue({
      id: 'entry-unread',
      mailbox_id: 'b1',
      message_id: 'msg-unread',
      uid: 1,
      internal_date: '2026-01-01T00:00:00Z',
      flags: ['\\Seen'],
    })

    const { rerender } = render(
      <MessageViewer
        message={detail({
          id: 'msg-unread',
          entries: [
            {
              entry_id: 'entry-unread',
              mailbox_id: 'b1',
              mailbox_name: 'INBOX',
              uid: 1,
              flags: [],
            },
          ],
        })}
        mailboxId="b1"
        onClose={() => {}}
      />,
      { wrapper },
    )

    // Unread message displays "Mark read" button
    const markReadBtn = screen.getByRole('button', { name: /Mark read/i })
    expect(markReadBtn).toBeInTheDocument()

    // Clicking "Mark read" adds \Seen
    fireEvent.click(markReadBtn)
    await waitFor(() => {
      expect(setFlagsSpy).toHaveBeenCalledWith('entry-unread', { add: ['\\Seen'] })
    })

    // Re-render as read
    rerender(
      <MessageViewer
        message={detail({
          id: 'msg-unread',
          entries: [
            {
              entry_id: 'entry-unread',
              mailbox_id: 'b1',
              mailbox_name: 'INBOX',
              uid: 1,
              flags: ['\\Seen'],
            },
          ],
        })}
        mailboxId="b1"
        onClose={() => {}}
      />,
    )

    // Read message displays "Mark unread" button
    const markUnreadBtn = screen.getByRole('button', { name: /Mark unread/i })
    expect(markUnreadBtn).toBeInTheDocument()

    // Clicking "Mark unread" removes \Seen
    fireEvent.click(markUnreadBtn)
    await waitFor(() => {
      expect(setFlagsSpy).toHaveBeenCalledWith('entry-unread', { remove: ['\\Seen'] })
    })
  })
})

