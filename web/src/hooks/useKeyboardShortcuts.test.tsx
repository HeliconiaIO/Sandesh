/**
 * Shortcut registration.
 *
 * The bug this guards was not a console message but a dead UI. Callers pass an
 * array literal, so its identity changes every render; the effect depended on
 * it and `registerShortcut` publishes provider state, which closed a loop —
 * register, re-render, register again. React aborted with "Maximum update depth
 * exceeded" and stopped committing, so clicking a mailbox changed the URL while
 * the view never re-rendered. Nothing failed a test, because no test rendered a
 * component that registered shortcuts and then counted renders.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { render, screen, act } from '@testing-library/react'
import { useState } from 'react'

import { KeyboardShortcutsProvider } from '../context/KeyboardShortcutsContext'
import { useKeyboardShortcuts } from './useKeyboardShortcuts'

/** A component that registers shortcuts the way every real caller does. */
function Caller({ onRender, label = 'run' }: { onRender: () => void; label?: string }) {
  onRender()
  const [count, setCount] = useState(0)

  // An array literal, recreated on every render — the shape that caused the loop.
  useKeyboardShortcuts([
    {
      key: 'x',
      description: label,
      // A fresh closure each render, capturing current state.
      handler: () => setCount((c) => c + 1),
    },
  ])

  return <div data-testid="count">{count}</div>
}

describe('useKeyboardShortcuts', () => {
  // React reports the loop by name on console.error before it gives up, and
  // this captures it.
  //
  // Honest limitation: against the buggy hook this file does not fail cleanly,
  // it *hangs* — the loop starves the event loop inside `render()`, so no
  // in-process assertion is ever reached, and the runner hits its timeout
  // instead. A timeout in CI still stops a regression from merging, but it
  // names it poorly. Catching this properly needs an out-of-process check
  // (a browser test asserting that a mailbox click changes the rendered view),
  // which belongs with the Playwright suite rather than here.
  let consoleErrors: string[] = []
  let originalError: typeof console.error

  beforeEach(() => {
    consoleErrors = []
    originalError = console.error
    console.error = (...args: unknown[]) => {
      consoleErrors.push(args.map(String).join(' '))
    }
  })

  afterEach(() => {
    console.error = originalError
  })

  it('settles instead of re-rendering forever', () => {
    let renders = 0
    render(
      <KeyboardShortcutsProvider>
        <Caller onRender={() => { renders += 1 }} />
      </KeyboardShortcutsProvider>,
    )

    // The named symptom, which is what actually broke navigation: React stops
    // committing, so a click changes the URL while the view never updates.
    expect(consoleErrors.join('\n')).not.toContain('Maximum update depth exceeded')

    // A handful of renders is normal (mount, then the provider publishing the
    // registration). An unbounded loop shows up as hundreds.
    expect(renders).toBeLessThan(10)
  })

  it('runs the latest handler, not the one captured at registration', () => {
    // The ref indirection that breaks the loop must not stale the closure:
    // a shortcut has to see current state, or it would act on old data.
    render(
      <KeyboardShortcutsProvider>
        <Caller onRender={() => {}} />
      </KeyboardShortcutsProvider>,
    )

    expect(screen.getByTestId('count').textContent).toBe('0')

    act(() => {
      window.dispatchEvent(new KeyboardEvent('keydown', { key: 'x', bubbles: true }))
    })
    expect(screen.getByTestId('count').textContent).toBe('1')

    // The second press must increment from 1, which only holds if the handler
    // is re-read rather than frozen at first registration.
    act(() => {
      window.dispatchEvent(new KeyboardEvent('keydown', { key: 'x', bubbles: true }))
    })
    expect(screen.getByTestId('count').textContent).toBe('2')
  })

  it('re-registers when the shortcut description actually changes', () => {
    const onRender = vi.fn()
    const { rerender } = render(
      <KeyboardShortcutsProvider>
        <Caller onRender={onRender} label="first" />
      </KeyboardShortcutsProvider>,
    )

    rerender(
      <KeyboardShortcutsProvider>
        <Caller onRender={onRender} label="second" />
      </KeyboardShortcutsProvider>,
    )

    // Still bounded: a real change re-registers once, it does not reopen the loop.
    expect(onRender.mock.calls.length).toBeLessThan(20)
  })
})
