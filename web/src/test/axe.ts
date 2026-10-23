/**
 * Accessibility assertions, shared by the per-view suites.
 *
 * ## What this can and cannot check
 *
 * axe-core runs against jsdom, which implements no layout and no cascade. Two
 * families of rule are therefore switched off here rather than left to produce
 * answers that would be wrong either way:
 *
 * - **`color-contrast`** needs computed colour and a rendered box. jsdom
 *   resolves neither, so axe cannot evaluate it. Left on, it reports
 *   "incomplete" forever, which reads as a pass and hides the real gap: contrast
 *   is verified in the browser (Playwright), not here.
 * - **Geometry rules** (`target-size`) depend on element size, and
 *   `src/test/setup.ts` stubs every element to one fixed 1024x2000 rect so the
 *   virtualiser works. Any size answer they gave would describe the stub.
 *
 * What remains is the structural majority of WCAG: roles, names, labels,
 * relationships, landmarks, heading order, ARIA validity and duplicate ids.
 * Those are exactly the defects that survive a design review and reach users.
 */

import { axe, type JestAxeConfigureOptions } from 'jest-axe'

// `@types/jest-axe` declares `toHaveNoViolations` for Jest only, so Vitest's
// `expect` needs telling separately; without this the matcher works at runtime
// but `tsc` rejects every call site. `Matchers` is the documented extension
// point, and `Assertion` already extends it.
declare module 'vitest' {
  // The generic arity *and parameter names* must match Vitest's own
  // declaration exactly, or the interfaces do not merge (TS2428). `T` is
  // therefore unused here by necessity, not by oversight, and cannot be
  // renamed to `_T` to satisfy the lint rule.
  // eslint-disable-next-line @typescript-eslint/no-unused-vars
  interface Matchers<R extends void | Promise<void> = void | Promise<void>, T = unknown> {
    /** Asserts an axe result carries no violations. */
    toHaveNoViolations(): R
  }
}

/** Rules that jsdom cannot evaluate honestly. See the module comment. */
const UNEVALUABLE_IN_JSDOM = {
  'color-contrast': { enabled: false },
  'target-size': { enabled: false },
} as const

/**
 * The WCAG tags asserted. 2.1 AA is the project's stated bar
 * (`docs/UI_UX_DIRECTION.md`), and `best-practice` is deliberately excluded:
 * it mixes advisory opinion into a gate, and a gate that argues gets muted.
 */
const WCAG_AA: JestAxeConfigureOptions = {
  runOnly: ['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa'],
  rules: UNEVALUABLE_IN_JSDOM,
}

/**
 * Runs axe over a container and returns its results.
 *
 * Pair with `toHaveNoViolations` from `jest-axe`, registered in
 * `src/test/setup.ts`.
 */
export async function checkA11y(container: Element) {
  return axe(container, WCAG_AA)
}
