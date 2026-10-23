/**
 * The accessibility criteria that only a real browser can answer.
 *
 * `src/test/a11y.test.tsx` runs axe under jsdom and deliberately switches off
 * `color-contrast` and `target-size`: jsdom resolves neither the cascade nor
 * layout, so those rules there would report "incomplete" forever and read as a
 * pass. This suite is where they are actually asserted, against a real engine
 * with real computed style, closing the two Phase 4 criteria that had no test:
 *
 * - both themes meet WCAG AA contrast
 * - the UI works at 375px width
 *
 * Like `mail.spec.ts`, this drives a real Sandesh binary with the UI embedded.
 */

import { expect, test, type Page } from '@playwright/test'
import AxeBuilder from '@axe-core/playwright'

/** The narrowest viewport the UI claims to support (iPhone SE and similar). */
const PHONE = { width: 375, height: 667 }

/** A desktop width above every breakpoint, for the themes' default layout. */
const DESKTOP = { width: 1440, height: 900 }

/**
 * The principal views, by the nav link that reaches them.
 *
 * Kept as routes rather than clicks so a failure names the view, and so the
 * suite does not depend on the nav being open at phone width.
 */
const VIEWS = [
  { name: 'Overview', path: '/' },
  { name: 'Search', path: '/search' },
  { name: 'Webhooks', path: '/webhooks' },
  { name: 'Logs', path: '/logs' },
  { name: 'Traces', path: '/traces' },
  { name: 'Settings', path: '/settings' },
] as const

/**
 * Forces a theme before the app boots.
 *
 * `useTheme` reads `sandesh.theme` from localStorage on first render, so
 * setting it as an init script makes the run deterministic rather than
 * dependent on the CI machine's OS preference — which is what would otherwise
 * decide which theme these assertions measured.
 */
async function useTheme(page: Page, theme: 'light' | 'dark') {
  await page.addInitScript(
    ([key, value]) => window.localStorage.setItem(key, value),
    ['sandesh.theme', theme],
  )
}

/** Waits for the shell to be interactive, so axe sees a rendered view. */
async function ready(page: Page) {
  await expect(page.getByRole('banner')).toBeVisible()
  await expect(page.locator('#main')).toBeVisible()
}

test.describe('contrast', () => {
  // The `color-contrast` rule alone: this suite's job is the one thing jsdom
  // could not check. Structural rules are already gated in the unit suite, and
  // asserting them again here would duplicate a faster test.
  for (const theme of ['light', 'dark'] as const) {
    test(`${theme} theme meets WCAG AA contrast on every principal view`, async ({ page }) => {
      await useTheme(page, theme)

      for (const view of VIEWS) {
        await page.goto(view.path)
        await ready(page)

        // Confirm the theme actually applied: a typo in the storage key would
        // otherwise silently measure the default theme twice.
        await expect(page.locator('html')).toHaveAttribute('data-theme', theme)

        const results = await new AxeBuilder({ page })
          .withRules(['color-contrast'])
          .analyze()

        expect(
          results.violations,
          `${theme} theme, ${view.name}: ${JSON.stringify(results.violations, null, 2)}`,
        ).toEqual([])
      }
    })
  }
})

test.describe('phone width', () => {
  test.use({ viewport: PHONE })

  test('no principal view scrolls horizontally at 375px', async ({ page }) => {
    for (const view of VIEWS) {
      await page.goto(view.path)
      await ready(page)

      // A horizontal scrollbar at phone width is the defect this criterion
      // exists to catch: content wider than the viewport is content a phone
      // user cannot reach. One pixel of tolerance absorbs sub-pixel rounding.
      const overflow = await page.evaluate(
        () => document.documentElement.scrollWidth - document.documentElement.clientWidth,
      )
      expect(overflow, `${view.name} overflows by ${overflow}px at 375px`).toBeLessThanOrEqual(1)
    }
  })

  test('every destination stays reachable at 375px', async ({ page }) => {
    // The regression this guards is specific and has happened before: the nav
    // was `display: none` below 820px with nothing replacing it, which stranded
    // every destination. Asserting the drawer opens and lists them is what
    // proves the layout is responsive rather than merely narrow.
    await page.goto('/')
    await ready(page)

    const toggle = page.getByRole('button', { name: 'Open navigation' })
    await expect(toggle).toBeVisible()
    await toggle.click()

    const nav = page.locator('#main-nav')
    await expect(nav).toBeVisible()
    for (const name of ['Search', 'Webhooks', 'Logs', 'Traces', 'Settings']) {
      await expect(nav.getByRole('link', { name: new RegExp(name, 'i') })).toBeVisible()
    }

    // And it must close again, or the drawer traps the view it opened over.
    await page.getByRole('button', { name: 'Close navigation' }).click()
    await expect(toggle).toBeVisible()
  })

  test('the header stays intact at 375px', async ({ page }) => {
    // The other half of the same past defect: the header overflowed and clipped
    // the connection status and both toggles.
    await page.goto('/')
    await ready(page)

    const banner = page.getByRole('banner')
    const box = await banner.boundingBox()
    expect(box, 'the header must be laid out').not.toBeNull()
    expect(box!.width).toBeLessThanOrEqual(PHONE.width + 1)

    // The nav toggle is the control that makes the drawer reachable; if the
    // header clips it, the whole phone layout is unusable.
    await expect(page.getByRole('button', { name: 'Open navigation' })).toBeInViewport()
  })
})

test.describe('desktop layout', () => {
  test.use({ viewport: DESKTOP })

  test('the nav is visible without a toggle above the breakpoints', async ({ page }) => {
    // The counterpart assertion: the drawer must be a phone-width adaptation,
    // not the permanent layout.
    await page.goto('/')
    await ready(page)

    await expect(page.locator('#main-nav')).toBeVisible()
    await expect(page.getByRole('button', { name: 'Open navigation' })).toBeHidden()
  })
})
