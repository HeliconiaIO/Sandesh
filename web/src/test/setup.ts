import '@testing-library/jest-dom/vitest'
import { expect } from 'vitest'
import { toHaveNoViolations } from 'jest-axe'

// Registered globally so every suite can assert accessibility without repeating
// the wiring; `src/test/axe.ts` owns which rules actually run.
expect.extend(toHaveNoViolations)

/**
 * Give jsdom a viewport with real dimensions.
 *
 * jsdom reports every element as 0×0 and implements no layout, which is fine
 * until something computes what is *visible*. The virtualised message list does
 * exactly that: with a zero-height scroll container it correctly concludes no
 * row is on screen and renders none, so every list assertion fails against a
 * component that works in a browser.
 *
 * Stubbing the virtualiser away would be the wrong fix — the tests would then
 * assert against a list that does not exist in production. Instead the
 * measurements jsdom declines to compute are supplied here, so the real
 * windowing logic runs and the tests exercise the component that ships.
 *
 * The height is deliberately generous (2000px): tests assert on rows by text,
 * and a realistic 600px viewport would window most fixtures out of the DOM,
 * making assertions depend on scroll position rather than on behaviour.
 */
const VIEWPORT_HEIGHT = 2000
const VIEWPORT_WIDTH = 1024

Object.defineProperty(HTMLElement.prototype, 'clientHeight', {
  configurable: true,
  get(this: HTMLElement) {
    return VIEWPORT_HEIGHT
  },
})

Object.defineProperty(HTMLElement.prototype, 'clientWidth', {
  configurable: true,
  get(this: HTMLElement) {
    return VIEWPORT_WIDTH
  },
})

Object.defineProperty(HTMLElement.prototype, 'offsetHeight', {
  configurable: true,
  get(this: HTMLElement) {
    return VIEWPORT_HEIGHT
  },
})

Object.defineProperty(HTMLElement.prototype, 'offsetWidth', {
  configurable: true,
  get(this: HTMLElement) {
    return VIEWPORT_WIDTH
  },
})

HTMLElement.prototype.getBoundingClientRect = function getBoundingClientRect(): DOMRect {
  return {
    x: 0,
    y: 0,
    top: 0,
    left: 0,
    right: VIEWPORT_WIDTH,
    bottom: VIEWPORT_HEIGHT,
    width: VIEWPORT_WIDTH,
    height: VIEWPORT_HEIGHT,
    toJSON: () => ({}),
  } as DOMRect
}

/**
 * `@tanstack/react-virtual` observes the scroll element for resizes, and jsdom
 * has no `ResizeObserver`. A no-op is the honest stub: the observer only tells
 * the virtualiser to re-measure, and the measurements above never change.
 */
if (!('ResizeObserver' in globalThis)) {
  globalThis.ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver
}

/** jsdom implements neither, and the list calls both when navigating. */
if (!HTMLElement.prototype.scrollIntoView) {
  HTMLElement.prototype.scrollIntoView = function scrollIntoView() {}
}
if (!Element.prototype.scrollTo) {
  Element.prototype.scrollTo = function scrollTo() {}
}

/**
 * jsdom implements no `EventSource`, and the Logs view opens one to stream log
 * lines. A no-op class is the honest stub: it connects to nothing and emits
 * nothing, so a test sees the view's pre-stream state, which is exactly the
 * state a real browser shows before the first line arrives.
 */
if (!('EventSource' in globalThis)) {
  globalThis.EventSource = class {
    static readonly CONNECTING = 0
    static readonly OPEN = 1
    static readonly CLOSED = 2
    readonly readyState = 0
    readonly url = ''
    readonly withCredentials = false
    onopen: unknown = null
    onmessage: unknown = null
    onerror: unknown = null
    addEventListener() {}
    removeEventListener() {}
    dispatchEvent() {
      return false
    }
    close() {}
  } as unknown as typeof EventSource
}
