/**
 * Cross-platform detection and keyboard shortcut formatting utilities (§11).
 *
 * Provides OS-accurate symbols and modifier keys across macOS, Windows, and Linux.
 */

export type PlatformOS = 'mac' | 'windows' | 'linux' | 'other'

/**
 * Detect the client operating system.
 */
export function getPlatformOS(): PlatformOS {
  if (typeof window === 'undefined' || typeof navigator === 'undefined') {
    return 'other'
  }

  // Modern userAgentData API
  // @ts-expect-error - userAgentData is experimental in standard TS lib
  const navPlatform = navigator.userAgentData?.platform?.toLowerCase()
  if (navPlatform) {
    if (navPlatform.includes('mac') || navPlatform.includes('ios')) return 'mac'
    if (navPlatform.includes('win')) return 'windows'
    if (navPlatform.includes('linux') || navPlatform.includes('android')) return 'linux'
  }

  // Fallback to navigator.platform / userAgent
  const platform = (navigator.platform || navigator.userAgent || '').toLowerCase()
  if (platform.includes('mac') || platform.includes('iphone') || platform.includes('ipad')) {
    return 'mac'
  }
  if (platform.includes('win')) {
    return 'windows'
  }
  if (platform.includes('linux')) {
    return 'linux'
  }

  return 'other'
}

export const isMac = getPlatformOS() === 'mac'

/**
 * Formats a key representation according to the detected platform.
 * E.g., 'Mod' -> '⌘' on Mac, 'Ctrl' on Windows/Linux.
 */
export function formatKeyForPlatform(key: string, os: PlatformOS = getPlatformOS()): string {
  const isApple = os === 'mac'

  switch (key.toLowerCase()) {
    case 'mod':
    case 'meta':
    case 'cmd':
    case 'command':
      return isApple ? '⌘' : 'Ctrl'
    case 'ctrl':
    case 'control':
      return isApple ? '⌃' : 'Ctrl'
    case 'alt':
    case 'option':
      return isApple ? '⌥' : 'Alt'
    case 'shift':
      return isApple ? '⇧' : 'Shift'
    case 'enter':
    case 'return':
      return isApple ? '↵' : 'Enter'
    case 'backspace':
      return isApple ? '⌫' : 'Backspace'
    case 'delete':
      return isApple ? '⌦' : 'Delete'
    case 'escape':
    case 'esc':
      return 'Esc'
    case 'arrowup':
    case 'up':
      return '↑'
    case 'arrowdown':
    case 'down':
      return '↓'
    case 'arrowleft':
    case 'left':
      return '←'
    case 'arrowright':
    case 'right':
      return '→'
    default:
      return key.length === 1 ? key.toUpperCase() : key
  }
}

/**
 * Returns true if event target is an interactive input element where single-key
 * hotkeys must not be intercepted.
 */
export function isInputActive(target?: EventTarget | null): boolean {
  if (!target || !(target instanceof HTMLElement)) return false

  const tagName = target.tagName.toUpperCase()
  if (tagName === 'INPUT' || tagName === 'TEXTAREA' || tagName === 'SELECT') {
    return true
  }

  if (
    target.isContentEditable ||
    target.contentEditable === 'true' ||
    target.getAttribute('contenteditable') === 'true' ||
    target.getAttribute('contenteditable') === ''
  ) {
    return true
  }

  return Boolean(target.closest?.('[contenteditable="true"]'))
}

/**
 * Checks whether a KeyboardEvent matches a modifier combo (e.g. Mod+K or Shift+I).
 */
export function matchesModifier(
  event: KeyboardEvent,
  options: {
    mod?: boolean
    ctrl?: boolean
    meta?: boolean
    shift?: boolean
    alt?: boolean
  } = {},
  os: PlatformOS = getPlatformOS(),
): boolean {
  const isApple = os === 'mac'

  if (options.mod !== undefined) {
    const activeMod = isApple ? event.metaKey : event.ctrlKey
    if (Boolean(options.mod) !== activeMod) return false
  }

  if (options.ctrl !== undefined && options.ctrl !== event.ctrlKey) {
    return false
  }

  if (options.meta !== undefined && options.meta !== event.metaKey) {
    return false
  }

  if (options.shift !== undefined && options.shift !== event.shiftKey) {
    return false
  }

  if (options.alt !== undefined && options.alt !== event.altKey) {
    return false
  }

  return true
}
