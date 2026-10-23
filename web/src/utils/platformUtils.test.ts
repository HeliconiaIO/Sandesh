import { describe, expect, it } from 'vitest'
import {
  formatKeyForPlatform,
  isInputActive,
  matchesModifier,
} from './platformUtils'

describe('platformUtils', () => {
  it('formats keys accurately for macOS', () => {
    expect(formatKeyForPlatform('Mod', 'mac')).toBe('⌘')
    expect(formatKeyForPlatform('Alt', 'mac')).toBe('⌥')
    expect(formatKeyForPlatform('Shift', 'mac')).toBe('⇧')
    expect(formatKeyForPlatform('Enter', 'mac')).toBe('↵')
    expect(formatKeyForPlatform('Backspace', 'mac')).toBe('⌫')
    expect(formatKeyForPlatform('j', 'mac')).toBe('J')
  })

  it('formats keys accurately for Windows and Linux', () => {
    expect(formatKeyForPlatform('Mod', 'windows')).toBe('Ctrl')
    expect(formatKeyForPlatform('Alt', 'windows')).toBe('Alt')
    expect(formatKeyForPlatform('Shift', 'windows')).toBe('Shift')
    expect(formatKeyForPlatform('Enter', 'windows')).toBe('Enter')
    expect(formatKeyForPlatform('Backspace', 'windows')).toBe('Backspace')
    expect(formatKeyForPlatform('k', 'linux')).toBe('K')
  })

  it('detects when input or editable element is active', () => {
    const input = document.createElement('input')
    const textarea = document.createElement('textarea')
    const div = document.createElement('div')
    const editableDiv = document.createElement('div')
    editableDiv.contentEditable = 'true'

    expect(isInputActive(input)).toBe(true)
    expect(isInputActive(textarea)).toBe(true)
    expect(isInputActive(editableDiv)).toBe(true)
    expect(isInputActive(div)).toBe(false)
    expect(isInputActive(null)).toBe(false)
  })

  it('matches modifiers correctly per platform', () => {
    // Mac: Mod = metaKey
    const macModEvent = new KeyboardEvent('keydown', { metaKey: true, ctrlKey: false })
    expect(matchesModifier(macModEvent, { mod: true }, 'mac')).toBe(true)
    expect(matchesModifier(macModEvent, { mod: true }, 'windows')).toBe(false)

    // Windows/Linux: Mod = ctrlKey
    const winModEvent = new KeyboardEvent('keydown', { metaKey: false, ctrlKey: true })
    expect(matchesModifier(winModEvent, { mod: true }, 'windows')).toBe(true)
    expect(matchesModifier(winModEvent, { mod: true }, 'mac')).toBe(false)
  })
})
