import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from 'react'
import {
  formatKeyForPlatform,
  getPlatformOS,
  isInputActive,
  matchesModifier,
  type PlatformOS,
} from '../utils/platformUtils'

export type ShortcutCategory =
  | 'Global & Navigation'
  | 'Navigation'
  | 'Message List'
  | 'Actions'
  | 'Composer'
  | 'Inspector'
  | 'Global'

export interface ShortcutDefinition {
  id?: string
  /** Key or sequence. E.g. '?' or 'c' or 'Mod+K' or 'g i' or '*' 'a' */
  keys?: string[] | string
  key?: string
  chord?: string[]
  mod?: boolean
  shift?: boolean
  alt?: boolean
  label?: string
  description?: string
  category?: ShortcutCategory
  handler?: (e: KeyboardEvent) => void
  action?: (e: KeyboardEvent) => void
  /** If true, fires even when user is typing in an input/textarea (default: false) */
  allowInInputs?: boolean
  /** Disabled condition */
  disabled?: boolean
}

export interface RegisteredShortcut {
  id: string
  keys: string[]
  label: string
  category: ShortcutCategory
  handler?: ((e: KeyboardEvent) => void) | undefined
  allowInInputs?: boolean | undefined
  disabled?: boolean | undefined
}

interface KeyboardShortcutsContextValue {
  os: PlatformOS
  shortcutsModalOpen: boolean
  setShortcutsModalOpen: (open: boolean) => void
  openShortcutsModal: () => void
  closeShortcutsModal: () => void
  toggleShortcutsModal: () => void
  registerShortcut: (shortcut: ShortcutDefinition) => () => void
  registeredShortcuts: RegisteredShortcut[]
  pendingChord: string | null
  formatKey: (key: string) => string
}

const KeyboardShortcutsContext = createContext<KeyboardShortcutsContextValue | null>(null)

export function KeyboardShortcutsProvider({ children }: { children: ReactNode }) {
  const [shortcutsModalOpen, setShortcutsModalOpen] = useState(false)
  const [pendingChord, setPendingChord] = useState<string | null>(null)
  // `ReturnType<typeof setTimeout>` rather than `NodeJS.Timeout`: this is
  // browser code, and `tsconfig.app.json` scopes `types` to the DOM libs. The
  // Node namespace only ever resolved here by way of a transitive
  // `@types/node`, which is not something this file should depend on.
  const chordTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null)
  const shortcutsRef = useRef<Map<string, RegisteredShortcut>>(new Map())
  const [, setRerender] = useState(0)

  const os = getPlatformOS()

  const openShortcutsModal = useCallback(() => setShortcutsModalOpen(true), [])
  const closeShortcutsModal = useCallback(() => setShortcutsModalOpen(false), [])
  const toggleShortcutsModal = useCallback(() => setShortcutsModalOpen((prev) => !prev), [])

  const formatKey = useCallback(
    (key: string) => formatKeyForPlatform(key, os),
    [os],
  )

  const registerShortcut = useCallback((shortcut: ShortcutDefinition) => {
    let normalizedKeys: string[] = []
    if (shortcut.chord && shortcut.chord.length > 0) {
      normalizedKeys = [shortcut.chord.join(' ')]
    } else if (shortcut.keys) {
      normalizedKeys = Array.isArray(shortcut.keys) ? shortcut.keys : [shortcut.keys]
    } else if (shortcut.key) {
      let combo = shortcut.key
      if (shortcut.shift && !combo.toLowerCase().includes('shift')) combo = `shift+${combo}`
      if (shortcut.alt && !combo.toLowerCase().includes('alt')) combo = `alt+${combo}`
      if (shortcut.mod && !combo.toLowerCase().includes('mod')) combo = `mod+${combo}`
      normalizedKeys = [combo]
    }

    const regId =
      shortcut.id ||
      `${normalizedKeys.join('_')}_${Math.random().toString(36).slice(2, 8)}`

    const registered: RegisteredShortcut = {
      id: regId,
      keys: normalizedKeys,
      label: shortcut.label || shortcut.description || '',
      category: shortcut.category || 'Global',
      handler: shortcut.handler || shortcut.action,
      allowInInputs: shortcut.allowInInputs,
      disabled: shortcut.disabled,
    }

    shortcutsRef.current.set(regId, registered)
    setRerender((v) => v + 1)
    return () => {
      shortcutsRef.current.delete(regId)
      setRerender((v) => v + 1)
    }
  }, [])

  // Clear pending chord after 1.2s timeout
  const clearChord = useCallback(() => {
    if (chordTimerRef.current) {
      clearTimeout(chordTimerRef.current)
      chordTimerRef.current = null
    }
    setPendingChord(null)
  }, [])

  const setChord = useCallback(
    (chord: string) => {
      if (chordTimerRef.current) {
        clearTimeout(chordTimerRef.current)
      }
      setPendingChord(chord)
      chordTimerRef.current = setTimeout(() => {
        setPendingChord(null)
        chordTimerRef.current = null
      }, 1200)
    },
    [],
  )

  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      // Never intercept during IME composition
      if (event.isComposing) return

      const inInput = isInputActive(event.target)

      // Esc always closes the shortcuts modal if open
      if (event.key === 'Escape' && shortcutsModalOpen) {
        event.preventDefault()
        event.stopPropagation()
        closeShortcutsModal()
        return
      }

      // Check chord sequences first (e.g. 'g i', 'g s', '* a')
      if (pendingChord && !inInput) {
        const fullCombo = `${pendingChord} ${event.key.toLowerCase()}`
        for (const shortcut of shortcutsRef.current.values()) {
          if (shortcut.disabled) continue
          const matches = shortcut.keys.some((k) => k.toLowerCase() === fullCombo)
          if (matches && shortcut.handler) {
            event.preventDefault()
            shortcut.handler(event)
            clearChord()
            return
          }
        }
        clearChord()
      }

      // Check single key or chord-starter
      const isShift = event.shiftKey
      const isCtrl = event.ctrlKey
      const isMeta = event.metaKey
      const isAlt = event.altKey
      const hasModifiers = isCtrl || isMeta || isAlt

      // Question mark '?' toggles shortcut modal
      if (event.key === '?' && !inInput && !hasModifiers) {
        event.preventDefault()
        toggleShortcutsModal()
        return
      }

      // If user presses a chord starter like 'g' or '*' outside inputs
      if (!inInput && !hasModifiers && !isShift && (event.key === 'g' || event.key === '*')) {
        setChord(event.key)
        return
      }

      // Check registered shortcuts
      for (const shortcut of shortcutsRef.current.values()) {
        if (shortcut.disabled) continue
        if (inInput && !shortcut.allowInInputs) continue

        for (const keyDef of shortcut.keys) {
          const parts = keyDef.split('+').map((p) => p.trim())
          const lastPart = parts[parts.length - 1]
          if (!lastPart) continue
          const mainKey = lastPart.toLowerCase()
          const needsMod = parts.some((p) => ['mod', 'cmd', 'meta'].includes(p.toLowerCase()))
          const needsCtrl = parts.some((p) => p.toLowerCase() === 'ctrl')
          const needsAlt = parts.some((p) => ['alt', 'option'].includes(p.toLowerCase()))
          const needsShift = parts.some((p) => p.toLowerCase() === 'shift')

          // Check modifier match
          const modOpts: { mod?: boolean; ctrl?: boolean; alt?: boolean; shift?: boolean } = {}
          if (needsMod) modOpts.mod = true
          if (needsCtrl) modOpts.ctrl = true
          if (needsAlt) modOpts.alt = true
          if (needsShift) modOpts.shift = true

          if (!matchesModifier(event, modOpts, os)) {
            continue
          }

          // If shortcut didn't ask for mod/ctrl/alt, ensure event doesn't have them
          if (!needsMod && !needsCtrl && !needsAlt && hasModifiers) {
            continue
          }

          const eventKey = event.key.toLowerCase()
          if (
            eventKey === mainKey ||
            (mainKey === 'enter' && eventKey === 'enter') ||
            (mainKey === 'esc' && eventKey === 'escape') ||
            (mainKey === 'backspace' && (eventKey === 'backspace' || eventKey === 'delete')) ||
            (mainKey === 'delete' && (eventKey === 'delete' || eventKey === 'backspace')) ||
            (mainKey === 'up' && (eventKey === 'arrowup' || eventKey === 'k')) ||
            (mainKey === 'down' && (eventKey === 'arrowdown' || eventKey === 'j'))
          ) {
            if (shortcut.handler) {
              event.preventDefault()
              shortcut.handler(event)
              return
            }
          }
        }
      }
    }

    window.addEventListener('keydown', handleKeyDown)
    return () => {
      window.removeEventListener('keydown', handleKeyDown)
    }
  }, [
    shortcutsModalOpen,
    pendingChord,
    closeShortcutsModal,
    toggleShortcutsModal,
    clearChord,
    setChord,
    os,
  ])

  /**
   * Discard a pending chord timer when the provider goes away.
   *
   * This is deliberately its own effect with an empty dependency list. It used
   * to live in the `keydown` effect's cleanup, which was a bug rather than a
   * tidier arrangement: that effect depends on `pendingChord`, so arming a
   * chord re-ran it, and the cleanup then cleared the very timer `setChord`
   * had just set. A pending chord therefore never expired — a `g` pressed and
   * abandoned would still navigate when an `i` was typed an hour later.
   */
  useEffect(() => {
    return () => {
      if (chordTimerRef.current) clearTimeout(chordTimerRef.current)
    }
  }, [])

  const registeredShortcuts = Array.from(shortcutsRef.current.values())

  return (
    <KeyboardShortcutsContext.Provider
      value={{
        os,
        shortcutsModalOpen,
        setShortcutsModalOpen,
        openShortcutsModal,
        closeShortcutsModal,
        toggleShortcutsModal,
        registerShortcut,
        registeredShortcuts,
        pendingChord,
        formatKey,
      }}
    >
      {children}
    </KeyboardShortcutsContext.Provider>
  )
}

const defaultContextValue: KeyboardShortcutsContextValue = {
  os: getPlatformOS(),
  shortcutsModalOpen: false,
  setShortcutsModalOpen: () => {},
  openShortcutsModal: () => {},
  closeShortcutsModal: () => {},
  toggleShortcutsModal: () => {},
  registerShortcut: () => () => {},
  registeredShortcuts: [],
  pendingChord: null,
  formatKey: (k: string) => formatKeyForPlatform(k, getPlatformOS()),
}

export function useKeyboardShortcutsContext(): KeyboardShortcutsContextValue {
  const ctx = useContext(KeyboardShortcutsContext)
  return ctx ?? defaultContextValue
}
