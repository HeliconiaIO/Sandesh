import { useEffect, useMemo, useRef } from 'react'
import {
  useKeyboardShortcutsContext,
  type ShortcutDefinition,
} from '../context/KeyboardShortcutsContext'

/**
 * Registers one or more keyboard shortcuts for the lifetime of the calling component.
 *
 * ## Why this does not depend on the array it is given
 *
 * Callers pass an array literal, so its identity changes on every render, and
 * `registerShortcut` updates provider state to publish the new registration.
 * Depending on that array therefore closed a loop — register, re-render, new
 * array, register again — which React aborted with "Maximum update depth
 * exceeded". The visible symptom was not a console message but a **dead UI**:
 * React stops committing, so clicking a mailbox changed the URL while the view
 * never re-rendered.
 *
 * The effect keys on the shortcuts' *description* instead: the keys, label and
 * flags that decide what is registered. Handlers are deliberately excluded,
 * because a handler is a fresh closure on every render and would reintroduce
 * the loop. They are read through a ref at dispatch time, so a shortcut always
 * runs the latest closure and still sees current props and state.
 */
export function useKeyboardShortcuts(shortcuts: ShortcutDefinition[]): void {
  const { registerShortcut } = useKeyboardShortcutsContext()

  // The latest definitions, readable from a handler registered long before.
  const latest = useRef(shortcuts)
  latest.current = shortcuts

  // Everything that determines what gets registered, minus the functions.
  const signature = useMemo(
    () =>
      JSON.stringify(
        shortcuts.map((s) => [
          s.id,
          s.key,
          s.keys,
          s.chord,
          s.mod,
          s.shift,
          s.alt,
          s.label,
          s.description,
          s.category,
          s.allowInInputs,
          s.disabled,
        ]),
      ),
    [shortcuts],
  )

  useEffect(() => {
    const unregisters = latest.current.map((shortcut, index) => {
      // `action` is dropped rather than set to undefined: the provider treats
      // it as an alias for `handler`, and under `exactOptionalPropertyTypes`
      // an explicit undefined is not the same as an absent key.
      const { action: _action, ...rest } = shortcut
      return registerShortcut({
        ...rest,
        // Dispatch through the ref so the handler is never stale, without the
        // handler's identity forcing a re-registration.
        handler: (event) => {
          const current = latest.current[index]
          const run = current?.handler ?? current?.action
          run?.(event)
        },
      })
    })
    return () => {
      unregisters.forEach((unreg) => unreg())
    }
  }, [signature, registerShortcut])
}
