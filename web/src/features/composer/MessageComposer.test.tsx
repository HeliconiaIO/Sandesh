import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { ReactNode } from 'react'

import { DRAFT_STORAGE_KEY, MessageComposer } from './MessageComposer'
import { DialogProvider } from '../../context/DialogContext'
import * as resources from '../../api/resources'

let client: QueryClient

function wrapper({ children }: { children: ReactNode }) {
  return (
    <QueryClientProvider client={client}>
      <DialogProvider>{children}</DialogProvider>
    </QueryClientProvider>
  )
}

beforeEach(() => {
  localStorage.clear()
  client = new QueryClient({
    defaultOptions: {
      queries: { retry: false },
    },
  })
  vi.spyOn(resources.accounts, 'list').mockResolvedValue({
    items: [
      {
        id: 'acc-1',
        username: 'alice',
        addresses: ['alice@sandesh.test'],
        is_catch_all: false,
        has_password: false,
      },
    ],
    next_cursor: null,
    total_estimate: 1,
  })
})

afterEach(() => {
  vi.restoreAllMocks()
  localStorage.clear()
})

describe('MessageComposer & RichTextEditor', () => {
  it('renders standard plain text textarea by default', () => {
    render(<MessageComposer isOpen={true} onClose={() => {}} />, { wrapper })

    expect(screen.getByPlaceholderText('Write message body here...')).toBeInTheDocument()
    expect(screen.queryByRole('toolbar', { name: 'Formatting options' })).not.toBeInTheDocument()
  })

  it('switches to RichTextEditor when HTML tab is clicked', () => {
    render(<MessageComposer isOpen={true} onClose={() => {}} />, { wrapper })

    const htmlTab = screen.getByRole('button', { name: 'HTML' })
    expect(htmlTab).not.toHaveClass('composer-segmented-btn--active')

    fireEvent.click(htmlTab)
    expect(htmlTab).toHaveClass('composer-segmented-btn--active')

    // Formatting toolbar should now be present
    expect(screen.getByRole('toolbar', { name: 'Formatting options' })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Bold' })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Italic' })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Underline' })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'HTML Source' })).toBeInTheDocument()
  })

  it('toggles between Visual and HTML source code modes', () => {
    render(
      <MessageComposer
        isOpen={true}
        initialValues={{ isHtml: true, body: '<p>Hello <strong>Sandesh</strong></p>' }}
        onClose={() => {}}
      />,
      { wrapper },
    )

    expect(screen.getByRole('toolbar', { name: 'Formatting options' })).toBeInTheDocument()

    // Click HTML source code toggle button
    const sourceBtn = screen.getByRole('button', { name: 'HTML Source' })
    fireEvent.click(sourceBtn)

    // Raw HTML textarea editor is rendered
    const sourceTextarea = screen.getByLabelText('Raw HTML source')
    expect(sourceTextarea).toBeInTheDocument()

    // Click again to toggle back to Visual view
    const visualBtn = screen.getByRole('button', { name: 'Visual Editor' })
    fireEvent.click(visualBtn)
    expect(screen.queryByLabelText('Raw HTML source')).not.toBeInTheDocument()
  })

  it('does NOT close when clicking the overlay backdrop outside the composer', () => {
    const onClose = vi.fn()
    render(<MessageComposer isOpen={true} onClose={onClose} />, { wrapper })

    const overlay = screen.getByRole('dialog')
    fireEvent.click(overlay)

    // onClose must NOT be called on backdrop click
    expect(onClose).not.toHaveBeenCalled()
    expect(screen.getByText('New Message')).toBeInTheDocument()
  })

  it('supports minimizing and expanding the composer window', () => {
    render(<MessageComposer isOpen={true} onClose={() => {}} />, { wrapper })

    const minimizeBtn = screen.getByRole('button', { name: 'Minimize composer' })
    fireEvent.click(minimizeBtn)

    // Should now show expand button and have minimized overlay class
    expect(screen.getByRole('button', { name: 'Expand composer' })).toBeInTheDocument()
    expect(screen.queryByPlaceholderText('Write message body here...')).not.toBeInTheDocument()

    // Expand back
    fireEvent.click(screen.getByRole('button', { name: 'Expand composer' }))
    expect(screen.getByPlaceholderText('Write message body here...')).toBeInTheDocument()
  })

  it('auto-saves draft to localStorage and restores it upon reopen', () => {
    const { unmount } = render(<MessageComposer isOpen={true} onClose={() => {}} />, { wrapper })

    const toInput = screen.getByLabelText('To')
    const subjectInput = screen.getByLabelText('Subject')
    const bodyInput = screen.getByPlaceholderText('Write message body here...')

    fireEvent.change(toInput, { target: { value: 'bob@example.com' } })
    fireEvent.change(subjectInput, { target: { value: 'Important Test' } })
    fireEvent.change(bodyInput, { target: { value: 'Hello from draft' } })

    // Verify localStorage has the draft saved
    const raw = localStorage.getItem(DRAFT_STORAGE_KEY)
    expect(raw).toBeTruthy()
    expect(JSON.parse(raw!)).toMatchObject({
      to: 'bob@example.com',
      subject: 'Important Test',
      body: 'Hello from draft',
    })

    // Unmount (simulating close or focus loss) and reopen
    unmount()

    render(<MessageComposer isOpen={true} onClose={() => {}} />, { wrapper })

    // Restores automatically
    expect(screen.getByText('Restored saved draft')).toBeInTheDocument()
    expect(screen.getByLabelText('To')).toHaveValue('bob@example.com')
    expect(screen.getByLabelText('Subject')).toHaveValue('Important Test')
    expect(screen.getByPlaceholderText('Write message body here...')).toHaveValue('Hello from draft')
  })

  it('prompts before discarding unsaved changes on close', async () => {
    const onClose = vi.fn()

    render(<MessageComposer isOpen={true} onClose={onClose} />, { wrapper })

    fireEvent.change(screen.getByLabelText('Subject'), { target: { value: 'Work in progress' } })

    const closeBtn = screen.getByRole('button', { name: 'Close composer' })
    fireEvent.click(closeBtn)

    // Custom confirmation dialog is displayed
    expect(screen.getByRole('alertdialog')).toBeInTheDocument()
    expect(screen.getByText('Discard draft?')).toBeInTheDocument()

    // User cancels confirmation ("Keep Editing"), modal must not close
    const cancelBtn = screen.getByRole('button', { name: 'Keep Editing' })
    fireEvent.click(cancelBtn)
    expect(onClose).not.toHaveBeenCalled()

    // When user confirms discard
    fireEvent.click(closeBtn)
    expect(screen.getByRole('alertdialog')).toBeInTheDocument()
    const discardBtn = screen.getByRole('button', { name: 'Discard' })
    fireEvent.click(discardBtn)
    await waitFor(() => {
      expect(onClose).toHaveBeenCalled()
    })
  })

  it('accepts sender and recipient addresses with display names (RFC 5322)', async () => {
    const sendSpy = vi.spyOn(resources.messages, 'send').mockResolvedValue({
      id: 'msg-123',
    } as any)

    render(<MessageComposer isOpen={true} onClose={() => {}} />, { wrapper })

    fireEvent.change(screen.getByLabelText('From'), {
      target: { value: 'Alice Smith <alice@sandesh.test>' },
    })
    fireEvent.change(screen.getByLabelText('To'), {
      target: { value: 'Bob Builder <bob@example.com>, carol@example.com' },
    })
    fireEvent.change(screen.getByLabelText('Subject'), {
      target: { value: 'Testing display names' },
    })

    fireEvent.click(screen.getByRole('button', { name: /^send$/i }))

    await waitFor(() => {
      expect(sendSpy).toHaveBeenCalledWith(
        expect.objectContaining({
          from: 'Alice Smith <alice@sandesh.test>',
          to: ['Bob Builder <bob@example.com>', 'carol@example.com'],
          subject: 'Testing display names',
        }),
      )
    })
  })

  it('rejects email addresses without @ symbol', () => {
    render(<MessageComposer isOpen={true} onClose={() => {}} />, { wrapper })

    fireEvent.change(screen.getByLabelText('From'), {
      target: { value: 'alice@sandesh.test' },
    })
    fireEvent.change(screen.getByLabelText('To'), {
      target: { value: 'invalid-address-without-at' },
    })
    fireEvent.click(screen.getByRole('button', { name: /^send$/i }))

    expect(screen.getByRole('alert')).toHaveTextContent('not a valid email address')
  })
})
