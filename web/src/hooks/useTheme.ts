/**
 * Theme selection (§53).
 *
 * Three states: `system` follows the OS and is the default, `dark` and `light`
 * are explicit overrides. Only an explicit choice is persisted, so a user who
 * never opens Settings keeps tracking their OS forever rather than being frozen
 * at whatever they happened to boot with.
 */

import { useCallback, useEffect, useState } from 'react'

export type ThemePreference = 'system' | 'dark' | 'light'

const STORAGE_KEY = 'sandesh.theme'

function systemTheme(): 'dark' | 'light' {
  if (typeof window === 'undefined' || !window.matchMedia) return 'dark'
  return window.matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark'
}

function readStored(): ThemePreference {
  try {
    const value = localStorage.getItem(STORAGE_KEY)
    return value === 'dark' || value === 'light' ? value : 'system'
  } catch {
    // Storage can be denied outright in private mode; the default still works.
    return 'system'
  }
}

export function useTheme() {
  const [preference, setPreference] = useState<ThemePreference>(readStored)

  useEffect(() => {
    const apply = () => {
      const resolved = preference === 'system' ? systemTheme() : preference
      document.documentElement.dataset['theme'] = resolved
    }

    apply()

    try {
      if (preference === 'system') {
        localStorage.removeItem(STORAGE_KEY)
      } else {
        localStorage.setItem(STORAGE_KEY, preference)
      }
    } catch {
      // A theme that cannot be remembered is still a theme that works now.
    }

    if (preference !== 'system' || !window.matchMedia) return

    // Follow the OS while it is being followed.
    const media = window.matchMedia('(prefers-color-scheme: light)')
    media.addEventListener('change', apply)
    return () => media.removeEventListener('change', apply)
  }, [preference])

  const setTheme = useCallback((next: ThemePreference) => setPreference(next), [])

  return { preference, setTheme }
}
