/**
 * Message list behaviour (§65).
 *
 * These assert what a user can do and see, not how the component is built:
 * a refactor that preserves behaviour must not break them, and one that
 * breaks selection or hides the unread state must.
 */

import { describe, expect, it, vi } from 'vitest'
import { fireEvent, render, screen } from '@testing-library/react'

import { MessageList } from './MessageList'
import { summary } from '../../test/fixtures'

describe('MessageList', () => {
  it('renders one row per message with sender and subject', () => {
    render(
      <MessageList
        messages={[
          summary({ id: 'a', subject: 'First' }),
          summary({ id: 'b', subject: 'Second' }),
        ]}
        selectedId={undefined}
        onSelect={() => {}}
      />,
    )

    expect(screen.getAllByRole('option')).toHaveLength(2)
    expect(screen.getByText('First')).toBeInTheDocument()
    expect(screen.getByText('Second')).toBeInTheDocument()
    expect(screen.getAllByText('App').length).toBeGreaterThan(0)
  })

  it('marks a message unread when a placement lacks \\Seen', () => {
    // Unread is a per-placement fact, not a message-level one (ADR-009).
    render(
      <MessageList
        messages={[summary({ id: 'a', entries: [{ ...summary().entries[0]!, flags: [] }] })]}
        selectedId={undefined}
        onSelect={() => {}}
      />,
    )

    expect(screen.getByText('Unread')).toBeInTheDocument()
  })

  it('does not mark a message unread once every placement is \\Seen', () => {
    render(
      <MessageList
        messages={[
          summary({ id: 'a', entries: [{ ...summary().entries[0]!, flags: ['\\Seen'] }] }),
        ]}
        selectedId={undefined}
        onSelect={() => {}}
      />,
    )

    expect(screen.queryByText('Unread')).not.toBeInTheDocument()
  })

  it('reports the clicked message', () => {
    const onSelect = vi.fn()
    render(
      <MessageList
        messages={[summary({ id: 'wanted', subject: 'Click me' })]}
        selectedId={undefined}
        onSelect={onSelect}
      />,
    )

    fireEvent.click(screen.getByText('Click me'))
    expect(onSelect).toHaveBeenCalledWith('wanted', expect.anything())
  })

  it('moves the selection with the arrow keys', () => {
    const onSelect = vi.fn()
    render(
      <MessageList
        messages={[summary({ id: 'a' }), summary({ id: 'b' })]}
        selectedId="a"
        onSelect={onSelect}
      />,
    )

    fireEvent.keyDown(screen.getByRole('listbox'), { key: 'ArrowDown' })
    expect(onSelect).toHaveBeenCalledWith('b')
  })

  it('does not move past the end of the list', () => {
    const onSelect = vi.fn()
    render(
      <MessageList
        messages={[summary({ id: 'a' }), summary({ id: 'b' })]}
        selectedId="b"
        onSelect={onSelect}
      />,
    )

    fireEvent.keyDown(screen.getByRole('listbox'), { key: 'ArrowDown' })
    expect(onSelect).toHaveBeenCalledWith('b')
  })

  it('exposes the selected row to assistive technology', () => {
    render(
      <MessageList
        messages={[summary({ id: 'a', subject: 'Chosen' })]}
        selectedId="a"
        onSelect={() => {}}
      />,
    )

    expect(screen.getByRole('option', { selected: true })).toHaveTextContent('Chosen')
  })

  it('names the other mailboxes a message also sits in', () => {
    // A message is not owned by one mailbox (§23). Viewing INBOX must still
    // reveal that the same message is in Archive.
    const message = summary({
      id: 'multi',
      entries: [
        { ...summary().entries[0]!, mailbox_id: 'box-1', mailbox_name: 'INBOX' },
        {
          entry_id: 'e2',
          mailbox_id: 'box-2',
          mailbox_name: 'Archive',
          uid: 7,
          flags: [],
        },
      ],
    })

    render(
      <MessageList
        messages={[message]}
        selectedId={undefined}
        onSelect={() => {}}
        mailboxId="box-1"
      />,
    )

    expect(screen.getByText('Archive')).toBeInTheDocument()
    // The mailbox being viewed is not repeated back at the user.
    expect(screen.queryByText('INBOX')).not.toBeInTheDocument()
  })

  it('flags a message that did not parse cleanly', () => {
    // A parse warning is a feature of a mail-debugging tool, not an error.
    render(
      <MessageList
        messages={[summary({ id: 'bad', parse_ok: false })]}
        selectedId={undefined}
        onSelect={() => {}}
      />,
    )

    expect(screen.getByText('Parse warning')).toBeInTheDocument()
  })

  it('marks messages that carry attachments', () => {
    render(
      <MessageList
        messages={[summary({ id: 'att', has_attachments: true })]}
        selectedId={undefined}
        onSelect={() => {}}
      />,
    )

    expect(screen.getByText('Has attachments')).toBeInTheDocument()
  })
})

/**
 * Virtualisation (Phase 4 acceptance criterion: "the list stays smooth at
 * 10,000 messages").
 *
 * The criterion is about rendering cost, so these assert the property that
 * *causes* smoothness — that the DOM holds a bounded window rather than one
 * node per message — instead of timing a render, which would be a flaky
 * assertion about the machine rather than about the component.
 */
describe('MessageList virtualisation', () => {
  const manyMessages = (count: number) =>
    Array.from({ length: count }, (_, index) =>
      summary({ id: `m${index}`, subject: `Message ${index}` }),
    )

  it('renders a bounded window, not one row per message, at 10,000 messages', () => {
    render(
      <MessageList messages={manyMessages(10_000)} selectedId={undefined} onSelect={() => {}} />,
    )

    const rendered = screen.getAllByRole('option')
    // The exact count depends on the stubbed viewport height; what matters is
    // that it is bounded and nowhere near the total. Without virtualisation
    // this is 10,000 and the assertion fails.
    expect(rendered.length).toBeGreaterThan(0)
    expect(rendered.length).toBeLessThan(200)
  })

  it('tells assistive technology the real list size, not the mounted count', () => {
    // A listbox holding forty mounted children would otherwise announce
    // "3 of 40" in a list of ten thousand.
    render(
      <MessageList messages={manyMessages(10_000)} selectedId={undefined} onSelect={() => {}} />,
    )

    const [first] = screen.getAllByRole('option')
    expect(first).toHaveAttribute('aria-setsize', '10000')
    expect(first).toHaveAttribute('aria-posinset', '1')
  })

  it('keeps the scroll height of the whole list, so the scrollbar is honest', () => {
    // The spacer is what makes a virtualised list scrollable over its full
    // extent; without it the scrollbar describes only the mounted window.
    const { container } = render(
      <MessageList messages={manyMessages(1_000)} selectedId={undefined} onSelect={() => {}} />,
    )

    const spacer = container.querySelector('.messages__spacer')
    expect(spacer).not.toBeNull()
    const height = Number.parseInt((spacer as HTMLElement).style.height, 10)
    // 1000 rows at the ~64px estimate; far larger than any viewport.
    expect(height).toBeGreaterThan(10_000)
  })

  it('asks for the next page when the end of the loaded rows is rendered', () => {
    const onEndReached = vi.fn()
    render(
      <MessageList
        messages={manyMessages(10)}
        selectedId={undefined}
        onSelect={() => {}}
        hasMore
        onEndReached={onEndReached}
      />,
    )

    // All ten fit in the stubbed viewport, so the last row is rendered and
    // paging must fire — this is what makes infinite scroll work rather than
    // stopping at the first page.
    expect(onEndReached).toHaveBeenCalled()
  })

  it('does not ask for another page when there is none', () => {
    const onEndReached = vi.fn()
    render(
      <MessageList
        messages={manyMessages(10)}
        selectedId={undefined}
        onSelect={() => {}}
        hasMore={false}
        onEndReached={onEndReached}
      />,
    )

    expect(onEndReached).not.toHaveBeenCalled()
  })

  it('still moves selection by keyboard in a virtualised list', () => {
    // Keyboard navigation must reach rows the window has not mounted; the row
    // is scrolled into view first so `aria-activedescendant` names a real
    // element rather than an id that is not in the DOM.
    const onSelect = vi.fn()
    render(
      <MessageList messages={manyMessages(10_000)} selectedId={undefined} onSelect={onSelect} />,
    )

    fireEvent.keyDown(screen.getByRole('listbox'), { key: 'ArrowDown' })
    expect(onSelect).toHaveBeenCalledWith('m0')

    fireEvent.keyDown(screen.getByRole('listbox'), { key: 'End' })
    expect(onSelect).toHaveBeenCalledWith('m9999')
  })

  it('triggers onLongPress on touch hold', () => {
    vi.useFakeTimers()
    const onLongPress = vi.fn()
    render(
      <MessageList
        messages={[summary({ id: 'a', subject: 'Hold me' })]}
        selectedId={undefined}
        onSelect={() => {}}
        onLongPress={onLongPress}
      />,
    )

    const row = screen.getByRole('option', { name: /Hold me/i })
    fireEvent.touchStart(row, { touches: [{ clientX: 100, clientY: 100 }] })
    vi.advanceTimersByTime(500)
    expect(onLongPress).toHaveBeenCalledWith('a')
    vi.useRealTimers()
  })

  it('renders multi-selected rows with selected class and aria-selected without checkboxes', () => {
    const msg = summary({ id: 'a', subject: 'Selected message' })
    const entryId = msg.entries[0]!.entry_id
    render(
      <MessageList
        messages={[msg]}
        selectedId={undefined}
        onSelect={() => {}}
        selectedEntryIds={new Set([entryId])}
      />,
    )

    const row = screen.getByRole('option', { name: /Selected message/i })
    expect(row).toHaveAttribute('aria-selected', 'true')
    expect(row).toHaveClass('message-row--selected')
    expect(screen.queryByRole('checkbox')).not.toBeInTheDocument()
  })
})
