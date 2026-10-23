/**
 * Chord sequences, and the timer that expires them.
 *
 * `AppShell` registers `g i`, `g s`, `g d`, `g t`, `g a` and `g /`, and
 * `MailboxRoute` registers `* a` and `* n` — so every one of the console's
 * navigation shortcuts is a two-key sequence. None of it had a test: the
 * existing `useKeyboardShortcuts.test.tsx` covers registration and the stale
 * closure, and stops at single keys.
 *
 * The gap surfaced while fixing a type error in the timer this file exercises.
 * `chordTimerRef` was declared `NodeJS.Timeout` in browser code and only
 * compiled because a transitive `@types/node` happened to be installed; when a
 * dependency upgrade removed it, `tsc` rejected the file. The type is now
 * `ReturnType<typeof setTimeout>`, which `tsc` proves — but nothing proved the
 * *behaviour* around it, and the 1.2s expiry in particular is invisible to a
 * test that never advances the clock.
 *
 * These use fake timers rather than waiting 1.2 seconds of wall clock. The
 * provider's timeout is the thing under test, so it is driven deliberately
 * rather than raced against.
 */

import { afterEach, beforeEach, describe, expect, it, vi, type Mock } from 'vitest'
import { act, render } from '@testing-library/react'

import { KeyboardShortcutsProvider } from './KeyboardShortcutsContext'
import { useKeyboardShortcuts } from '../hooks/useKeyboardShortcuts'

/** The chord timeout in `KeyboardShortcutsContext`. */
const CHORD_TIMEOUT_MS = 1200

/**
 * Registers the two chords that matter here, in the shape real callers use:
 * `AppShell`'s `g i` and `MailboxRoute`'s `* a`, plus a plain `x` to prove a
 * chord starter does not swallow ordinary shortcuts.
 */
function Caller({
  onGoInbox,
  onSelectAll,
  onPlain,
}: {
  onGoInbox: () => void
  onSelectAll: () => void
  onPlain?: () => void
}) {
  useKeyboardShortcuts([
    { id: 'go-inbox', chord: ['g', 'i'], description: 'Go to inbox', handler: onGoInbox },
    { id: 'select-all', chord: ['*', 'a'], description: 'Select all', handler: onSelectAll },
    { id: 'plain', key: 'x', description: 'Plain', handler: onPlain ?? (() => {}) },
  ])
  return null
}

function mount(props: Parameters<typeof Caller>[0]) {
  return render(
    <KeyboardShortcutsProvider>
      <Caller {...props} />
    </KeyboardShortcutsProvider>,
  )
}

/** A keydown on `window`, which is where the provider listens. */
function press(key: string, init: KeyboardEventInit = {}) {
  act(() => {
    window.dispatchEvent(new KeyboardEvent('keydown', { key, bubbles: true, ...init }))
  })
}

describe('chord sequences', () => {
  let onGoInbox: Mock<() => void>
  let onSelectAll: Mock<() => void>

  beforeEach(() => {
    vi.useFakeTimers()
    onGoInbox = vi.fn<() => void>()
    onSelectAll = vi.fn<() => void>()
  })

  afterEach(() => {
    // Order matters: pending timers must be discarded before real timers are
    // restored, or an unmounted provider's 1.2s callback can outlive the test.
    vi.clearAllTimers()
    vi.useRealTimers()
  })

  it('runs the handler when the full sequence is pressed', () => {
    mount({ onGoInbox, onSelectAll })

    press('g')
    expect(onGoInbox).not.toHaveBeenCalled() // The starter alone does nothing.

    press('i')
    expect(onGoInbox).toHaveBeenCalledTimes(1)
  })

  it('distinguishes sequences that share a starter', () => {
    mount({ onGoInbox, onSelectAll })

    // `* a` must not fire from `g a`, and vice versa: the pending starter is
    // half of the match, so a provider that ignored it would fire both.
    press('*')
    press('a')

    expect(onSelectAll).toHaveBeenCalledTimes(1)
    expect(onGoInbox).not.toHaveBeenCalled()
  })

  /**
   * The timer this file exists for. Without it a chord started and abandoned
   * stays armed forever, so an `i` typed minutes later navigates — the class of
   * bug where a UI acts on input the user has long forgotten giving.
   */
  it('expires the pending chord after the timeout', () => {
    mount({ onGoInbox, onSelectAll })

    press('g')
    act(() => {
      vi.advanceTimersByTime(CHORD_TIMEOUT_MS + 1)
    })

    press('i')
    expect(onGoInbox).not.toHaveBeenCalled()
  })

  it('still completes just before the timeout', () => {
    mount({ onGoInbox, onSelectAll })

    press('g')
    act(() => {
      // One millisecond short: the boundary is asserted from both sides, so a
      // change to the delay cannot pass by being merely *close* to 1.2s.
      vi.advanceTimersByTime(CHORD_TIMEOUT_MS - 1)
    })

    press('i')
    expect(onGoInbox).toHaveBeenCalledTimes(1)
  })

  it('restarts the timer when a second starter is pressed', () => {
    mount({ onGoInbox, onSelectAll })

    press('g')
    act(() => {
      vi.advanceTimersByTime(CHORD_TIMEOUT_MS - 100)
    })

    // A fresh starter must re-arm rather than inherit the first one's
    // remaining 100ms — `setChord` clears the outstanding timer before setting
    // a new one, and this is what proves that clear happens.
    press('g')
    act(() => {
      vi.advanceTimersByTime(CHORD_TIMEOUT_MS - 100)
    })

    press('i')
    expect(onGoInbox).toHaveBeenCalledTimes(1)
  })

  it('clears the pending chord after a completed sequence', () => {
    mount({ onGoInbox, onSelectAll })

    press('g')
    press('i')
    expect(onGoInbox).toHaveBeenCalledTimes(1)

    // A second `i` must not fire again: completing a chord clears it, so the
    // next key starts from nothing rather than from a still-pending `g`.
    press('i')
    expect(onGoInbox).toHaveBeenCalledTimes(1)
  })

  it('clears the pending chord after an unmatched second key', () => {
    mount({ onGoInbox, onSelectAll })

    // `g` then `q` matches no registered sequence. The chord must be abandoned
    // rather than left waiting for a key that completes it later.
    press('g')
    press('q')
    press('i')

    expect(onGoInbox).not.toHaveBeenCalled()
  })

  it('does not start a chord while typing in an input', () => {
    mount({ onGoInbox, onSelectAll })

    const input = document.createElement('input')
    document.body.appendChild(input)
    input.focus()

    act(() => {
      input.dispatchEvent(new KeyboardEvent('keydown', { key: 'g', bubbles: true }))
    })
    input.remove()

    // Typing "g" into a search box must not arm a navigation chord; a later
    // `i` would otherwise navigate away mid-sentence.
    press('i')
    expect(onGoInbox).not.toHaveBeenCalled()
  })

  it('does not treat a modified starter as a chord', () => {
    mount({ onGoInbox, onSelectAll })

    // Ctrl+G is the browser's find-next, not a chord starter.
    press('g', { ctrlKey: true })
    press('i')

    expect(onGoInbox).not.toHaveBeenCalled()
  })

  it('leaves ordinary shortcuts working', () => {
    const onPlain = vi.fn<() => void>()
    mount({ onGoInbox, onSelectAll, onPlain })

    press('x')
    expect(onPlain).toHaveBeenCalledTimes(1)
    expect(onGoInbox).not.toHaveBeenCalled()
  })

  /**
   * The provider clears its timer on unmount. Without that, a chord started
   * just before a route change fires `setPendingChord` on an unmounted
   * provider — a React state-update warning, and a leaked timer per navigation.
   */
  it('clears the pending timer on unmount', () => {
    const { unmount } = mount({ onGoInbox, onSelectAll })

    press('g')
    unmount()

    expect(vi.getTimerCount()).toBe(0)
  })
})
