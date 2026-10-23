/**
 * The import/export view (Phase 8 A5).
 *
 * The assertions that matter are about what a developer can tell from the
 * screen after acting: how many messages actually landed, and — when the file
 * was refused — what the server said was wrong with it. A view that reported
 * "done" either way would look fine and answer neither, which is the same
 * silent-success failure the backend's corpus test exists to catch.
 */

import { describe, expect, it, vi, beforeEach } from 'vitest'
import { fireEvent, screen, waitFor } from '@testing-library/react'

import { renderWithProviders } from '../test/render'
import { TransferRoute } from './TransferRoute'

const importMessages = vi.fn()
const exportUrl = vi.fn()

vi.mock('../api/resources', () => ({
  transfer: {
    import: (...args: unknown[]) => importMessages(...args),
    exportUrl: (...args: unknown[]) => exportUrl(...args),
  },
  mailboxes: {
    list: vi.fn().mockResolvedValue({
      items: [{ id: '019a0000-0000-7000-8000-0000000000a1', name: 'INBOX' }],
    }),
  },
  accounts: { list: vi.fn().mockResolvedValue({ items: [] }) },
  messages: {},
  entries: {},
  health: {},
  search: {},
  connections: {},
  faults: {},
  webhooks: {},
}))

/** Selects `file` in the hidden file input, as choosing one in the OS dialog would. */
function choose(contents: string, name = 'corpus.mbox') {
  const input = document.getElementById('transfer-file') as HTMLInputElement
  const file = new File([contents], name, { type: 'application/mbox' })
  fireEvent.change(input, { target: { files: [file] } })
  return file
}

describe('TransferRoute', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    exportUrl.mockReturnValue('/api/v1/export?format=mbox')
  })

  it('names the framing variant, because mboxo is the one that loses data', async () => {
    renderWithProviders(<TransferRoute />)
    // The guarantee is the reason to trust the feature, so it is on screen
    // rather than in a doc comment.
    expect(await screen.findByText(/mboxrd/)).toBeInTheDocument()
  })

  it('offers the export as a download link the browser can stream', async () => {
    renderWithProviders(<TransferRoute />)

    const link = await screen.findByRole('link', { name: /download mbox/i })
    expect(link).toHaveAttribute('href', '/api/v1/export?format=mbox')
    expect(link).toHaveAttribute('download')
  })

  it('scopes the export to a chosen mailbox', async () => {
    // Asserted through the href the user actually gets, rather than through the
    // call that built it: the link is the thing that has to be right.
    exportUrl.mockReset()
    exportUrl.mockImplementation((options: { mailboxId?: string } = {}) =>
      options.mailboxId
        ? `/api/v1/export?format=mbox&mailbox_id=${options.mailboxId}`
        : '/api/v1/export?format=mbox',
    )

    renderWithProviders(<TransferRoute />)

    // Wait for the mailbox to arrive before selecting it: changing a select to
    // a value whose option has not rendered yet is a no-op.
    const select = await screen.findByLabelText('Mailbox')
    await screen.findAllByRole('option', { name: 'INBOX' })
    fireEvent.change(select, { target: { value: '019a0000-0000-7000-8000-0000000000a1' } })

    await waitFor(() => {
      expect(screen.getByRole('link', { name: /download mbox/i })).toHaveAttribute(
        'href',
        '/api/v1/export?format=mbox&mailbox_id=019a0000-0000-7000-8000-0000000000a1',
      )
    })
  })

  it('reports how many messages landed, not merely that it finished', async () => {
    importMessages.mockResolvedValue({ parsed: 3, imported: 3, message_ids: ['a', 'b', 'c'] })

    renderWithProviders(<TransferRoute />)
    await screen.findByLabelText('Mailbox')
    choose('From sandesh@localhost\r\n\r\n')

    expect(await screen.findByRole('status')).toHaveTextContent('Imported 3 messages.')
  })

  it('says how many of the file were found when some did not import', async () => {
    // The count a person needs when the two disagree is both numbers, not one.
    importMessages.mockResolvedValue({ parsed: 5, imported: 4, message_ids: ['a'] })

    renderWithProviders(<TransferRoute />)
    await screen.findByLabelText('Mailbox')
    choose('From sandesh@localhost\r\n\r\n')

    expect(await screen.findByRole('status')).toHaveTextContent(
      'Imported 4 messages of 5 found in the file.',
    )
  })

  it("passes the server's explanation through when a file is refused", async () => {
    const { ApiError } = await import('../api/client')
    importMessages.mockRejectedValue(
      new ApiError(
        400,
        'bad-request',
        'Malformed request',
        "no messages found; an mbox must begin with a 'From ' separator line",
      ),
    )

    renderWithProviders(<TransferRoute />)
    await screen.findByLabelText('Mailbox')
    choose('this is not an mbox')

    // Specific enough to act on: the fix is named, not just the failure.
    expect(await screen.findByRole('alert')).toHaveTextContent("'From ' separator line")
  })

  it('sends the chosen format with the file', async () => {
    importMessages.mockResolvedValue({ parsed: 1, imported: 1, message_ids: ['a'] })

    renderWithProviders(<TransferRoute />)
    const format = await screen.findByLabelText('Format')
    fireEvent.change(format, { target: { value: 'eml' } })

    choose('Subject: one\r\n\r\nbody\r\n', 'message.eml')

    await waitFor(() => {
      expect(importMessages).toHaveBeenCalledWith(expect.any(File), 'eml', undefined)
    })
  })

  it('sends the chosen destination mailbox with the file when specified', async () => {
    importMessages.mockResolvedValue({ parsed: 1, imported: 1, message_ids: ['a'] })

    renderWithProviders(<TransferRoute />)
    const destSelect = await screen.findByLabelText('Destination Mailbox')
    await screen.findAllByRole('option', { name: 'INBOX' })
    fireEvent.change(destSelect, { target: { value: '019a0000-0000-7000-8000-0000000000a1' } })

    choose('From sandesh@localhost\r\n\r\n')

    await waitFor(() => {
      expect(importMessages).toHaveBeenCalledWith(
        expect.any(File),
        'mbox',
        '019a0000-0000-7000-8000-0000000000a1',
      )
    })
  })
})
