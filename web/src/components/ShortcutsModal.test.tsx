import { describe, expect, it } from 'vitest'
import { fireEvent, render, screen } from '@testing-library/react'
import { KeyboardShortcutsProvider, useKeyboardShortcutsContext } from '../context/KeyboardShortcutsContext'
import { ShortcutsModal } from './ShortcutsModal'

function TestWrapper({ defaultOpen = false }: { defaultOpen?: boolean }) {
  const { openShortcutsModal } = useKeyboardShortcutsContext()
  return (
    <div>
      <button type="button" onClick={openShortcutsModal} data-testid="open-btn">
        Open
      </button>
      {defaultOpen && <span data-testid="ready" />}
      <ShortcutsModal />
    </div>
  )
}

describe('ShortcutsModal', () => {
  it('does not render modal content by default', () => {
    render(
      <KeyboardShortcutsProvider>
        <TestWrapper />
      </KeyboardShortcutsProvider>,
    )

    expect(screen.queryByRole('dialog')).toBeNull()
  })

  it('renders modal when opened and displays shortcut categories', () => {
    render(
      <KeyboardShortcutsProvider>
        <TestWrapper />
      </KeyboardShortcutsProvider>,
    )

    fireEvent.click(screen.getByTestId('open-btn'))

    expect(screen.getByRole('dialog')).toBeDefined()
    expect(screen.getByText('Keyboard Shortcuts')).toBeDefined()
    expect(screen.getByText('Global & Navigation')).toBeDefined()
    expect(screen.getByText('Message List')).toBeDefined()
    expect(screen.getByText('Actions')).toBeDefined()
    expect(screen.getByText('Inspector')).toBeDefined()
    expect(screen.getByText('Composer')).toBeDefined()
  })

  it('filters shortcuts when typing in search input', () => {
    render(
      <KeyboardShortcutsProvider>
        <TestWrapper />
      </KeyboardShortcutsProvider>,
    )

    fireEvent.click(screen.getByTestId('open-btn'))

    const searchInput = screen.getByPlaceholderText(/search hotkeys/i)
    fireEvent.change(searchInput, { target: { value: 'archive' } })

    // "Archive selected message(s)" should match
    expect(screen.getByText('Archive selected message(s)')).toBeDefined()
    // Other non-matching actions should not appear
    expect(screen.queryByText('Compose new message')).toBeNull()
  })

  it('shows empty state when no shortcuts match filter', () => {
    render(
      <KeyboardShortcutsProvider>
        <TestWrapper />
      </KeyboardShortcutsProvider>,
    )

    fireEvent.click(screen.getByTestId('open-btn'))

    const searchInput = screen.getByPlaceholderText(/search hotkeys/i)
    fireEvent.change(searchInput, { target: { value: 'xyz123nonsense' } })

    expect(screen.getByText(/no shortcuts match/i)).toBeDefined()
  })

  it('closes modal when clicking close button', () => {
    render(
      <KeyboardShortcutsProvider>
        <TestWrapper />
      </KeyboardShortcutsProvider>,
    )

    fireEvent.click(screen.getByTestId('open-btn'))
    expect(screen.getByRole('dialog')).toBeDefined()

    const closeBtn = screen.getByLabelText('Close shortcuts modal')
    fireEvent.click(closeBtn)

    expect(screen.queryByRole('dialog')).toBeNull()
  })

  it('closes modal on Escape key', () => {
    render(
      <KeyboardShortcutsProvider>
        <TestWrapper />
      </KeyboardShortcutsProvider>,
    )

    fireEvent.click(screen.getByTestId('open-btn'))
    expect(screen.getByRole('dialog')).toBeDefined()

    fireEvent.keyDown(window, { key: 'Escape' })
    expect(screen.queryByRole('dialog')).toBeNull()
  })
})
