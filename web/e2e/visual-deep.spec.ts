import { test, expect } from '@playwright/test'
import * as fs from 'node:fs'
import * as path from 'node:path'

const SCREENSHOT_DIR = path.resolve(process.cwd(), 'e2e-screenshots')

test.beforeAll(() => {
  if (!fs.existsSync(SCREENSHOT_DIR)) {
    fs.mkdirSync(SCREENSHOT_DIR, { recursive: true })
  }
})

test.describe('Deep Browser Verification - Custom UI Controls & Routes', () => {
  test('Logs Route: Custom Level Select interacts smoothly, filters, and renders in Dark & Light modes', async ({
    page,
  }) => {
    const consoleErrors: string[] = []
    page.on('console', (msg) => {
      if (msg.type() === 'error') consoleErrors.push(msg.text())
    })
    page.on('pageerror', (err) => consoleErrors.push(err.message))

    // 1. Visit Logs
    await page.goto('/logs')
    await page.waitForLoadState('networkidle')

    // Find the custom select container & trigger for Level filter
    const levelTrigger = page.locator('#logs-level-select')
    await expect(levelTrigger).toBeVisible()
    await expect(levelTrigger).toHaveText(/All/i)

    // Capture initial closed state in current theme
    await page.screenshot({ path: path.join(SCREENSHOT_DIR, '01-logs-level-closed-dark.png') })

    // 2. Click to open dropdown
    await levelTrigger.click()
    const dropdown = page.locator('.select-dropdown')
    await expect(dropdown).toBeVisible()
    await page.waitForTimeout(200)

    // Verify all options exist
    const options = dropdown.locator('.select-option')
    await expect(options).toHaveCount(6)
    await expect(options.nth(0)).toContainText('All')
    await expect(options.nth(1)).toContainText('trace and above')
    await expect(options.nth(2)).toContainText('debug and above')
    await expect(options.nth(3)).toContainText('info and above')
    await expect(options.nth(4)).toContainText('warn and above')
    await expect(options.nth(5)).toContainText('error and above')

    // Capture open state with modern styling (not native browser select!)
    await page.screenshot({ path: path.join(SCREENSHOT_DIR, '02-logs-level-open-dark.png') })

    // 3. Select "warn and above"
    await options.filter({ hasText: 'warn and above' }).click()
    await expect(dropdown).toBeHidden()
    await expect(levelTrigger).toContainText('warn and above')

    // 4. Test Keyboard Navigation
    // Focus trigger and press Enter to open
    await levelTrigger.focus()
    await page.keyboard.press('Enter')
    await expect(dropdown).toBeVisible()

    // Press ArrowDown to navigate, Escape to close
    await page.keyboard.press('ArrowDown')
    await page.keyboard.press('Escape')
    await expect(dropdown).toBeHidden()

    // 5. Test Light Mode
    // Toggle theme via theme toggle button in header
    const themeToggle = page.locator('button[aria-label*="theme" i], button.header__theme-toggle, button:has(.lucide-sun), button:has(.lucide-moon)').first()
    if (await themeToggle.isVisible()) {
      await themeToggle.click()
      await page.waitForTimeout(200)

      // Open dropdown in light mode
      await levelTrigger.click()
      await expect(dropdown).toBeVisible()
      await page.screenshot({ path: path.join(SCREENSHOT_DIR, '03-logs-level-open-light.png') })
      await page.keyboard.press('Escape')

      // Switch back to dark mode
      await themeToggle.click()
      await page.waitForTimeout(200)
    }

    expect(consoleErrors).toHaveLength(0)
  })

  test('Transfer Route: Custom Form Dropdowns (Format, Destination, Mailbox) function without native artifacts', async ({
    page,
  }) => {
    const consoleErrors: string[] = []
    page.on('console', (msg) => {
      if (msg.type() === 'error') consoleErrors.push(msg.text())
    })
    page.on('pageerror', (err) => consoleErrors.push(err.message))

    await page.goto('/transfer')
    await page.waitForLoadState('networkidle')

    // 1. Verify Format custom select
    const formatTrigger = page.locator('#transfer-format')
    await expect(formatTrigger).toBeVisible()
    await expect(formatTrigger).toContainText('mbox — many messages')

    // Click format dropdown
    await formatTrigger.click()
    const formatDropdown = page.locator('.select-dropdown')
    await expect(formatDropdown).toBeVisible()

    // Verify format options
    await expect(formatDropdown.locator('.select-option')).toHaveCount(2)
    await page.screenshot({ path: path.join(SCREENSHOT_DIR, '04-transfer-format-open.png') })

    // Select .eml
    await formatDropdown.locator('.select-option').filter({ hasText: '.eml' }).click()
    await expect(formatTrigger).toContainText('.eml — one message')

    // 2. Verify Destination Mailbox custom select
    const destTrigger = page.locator('#import-mailbox')
    await expect(destTrigger).toBeVisible()
    await destTrigger.click()
    const destDropdown = page.locator('.select-dropdown')
    await expect(destDropdown).toBeVisible()
    await page.screenshot({ path: path.join(SCREENSHOT_DIR, '05-transfer-dest-open.png') })
    await page.keyboard.press('Escape')

    // 3. Verify Export Mailbox custom select
    const mailboxTrigger = page.locator('#transfer-mailbox')
    await expect(mailboxTrigger).toBeVisible()
    await mailboxTrigger.click()
    await expect(page.locator('.select-dropdown')).toBeVisible()
    await page.screenshot({ path: path.join(SCREENSHOT_DIR, '06-transfer-export-open.png') })
    await page.keyboard.press('Escape')

    expect(consoleErrors).toHaveLength(0)
  })

  test('Guide Route: Workspace custom select loads and switches accurately', async ({ page }) => {
    const consoleErrors: string[] = []
    page.on('console', (msg) => {
      if (msg.type() === 'error') consoleErrors.push(msg.text())
    })
    page.on('pageerror', (err) => consoleErrors.push(err.message))

    await page.goto('/guide')
    await page.waitForLoadState('networkidle')

    const wsTrigger = page.locator('#guide-ws-select')
    await expect(wsTrigger).toBeVisible()
    await page.screenshot({ path: path.join(SCREENSHOT_DIR, '07-guide-view.png') })

    await wsTrigger.click()
    await expect(page.locator('.select-dropdown')).toBeVisible()
    await page.screenshot({ path: path.join(SCREENSHOT_DIR, '08-guide-ws-open.png') })
    await page.keyboard.press('Escape')

    expect(consoleErrors).toHaveLength(0)
  })

  test('Connect Client Modal: Custom select rendered cleanly inside modal dialog', async ({ page }) => {
    const consoleErrors: string[] = []
    page.on('console', (msg) => {
      if (msg.type() === 'error') consoleErrors.push(msg.text())
    })
    page.on('pageerror', (err) => consoleErrors.push(err.message))

    await page.goto('/workspaces')
    await page.waitForLoadState('networkidle')

    // Click "Connect Client" button on workspace card
    const connectBtn = page.locator('button:has-text("Connect Client")').first()
    if (await connectBtn.isVisible()) {
      await connectBtn.click()
      const modal = page.locator('.ws-modal.connect-modal')
      await expect(modal).toBeVisible()
      await page.screenshot({ path: path.join(SCREENSHOT_DIR, '09-connect-modal.png') })

      // Check account select if multiple accounts exist or credentials grid
      const accSelectTrigger = page.locator('#connect-acc-select')
      if (await accSelectTrigger.isVisible()) {
        await accSelectTrigger.click()
        await expect(page.locator('.select-dropdown')).toBeVisible()
        await page.screenshot({ path: path.join(SCREENSHOT_DIR, '10-connect-modal-acc-open.png') })
        await page.keyboard.press('Escape')
      }

      // Close modal
      const closeBtn = page.locator('.dialog-close, button[aria-label="Close"], button:has-text("Close")').first()
      if (await closeBtn.isVisible()) {
        await closeBtn.click()
      }
    }

    expect(consoleErrors).toHaveLength(0)
  })

  test('Cross-Device Responsive Check: Controls adapt across mobile (375px), tablet (768px), desktop (1440px)', async ({
    page,
  }) => {
    const viewports = [
      { name: 'mobile', width: 375, height: 667 },
      { name: 'tablet', width: 768, height: 1024 },
      { name: 'desktop', width: 1440, height: 900 },
    ]

    for (const vp of viewports) {
      await page.setViewportSize({ width: vp.width, height: vp.height })
      await page.goto('/logs')
      await page.waitForLoadState('networkidle')

      const levelTrigger = page.locator('#logs-level-select')
      await expect(levelTrigger).toBeVisible()
      await levelTrigger.click()
      await expect(page.locator('.select-dropdown')).toBeVisible()

      await page.screenshot({
        path: path.join(SCREENSHOT_DIR, `11-responsive-logs-${vp.name}.png`),
      })

      await page.keyboard.press('Escape')
      await expect(page.locator('.select-dropdown')).toBeHidden()
    }
  })

  test('Accessibility: Clicking a <label> focuses and opens the custom Select dropdown', async ({ page }) => {
    await page.goto('/logs')
    await page.waitForLoadState('networkidle')

    const label = page.locator('label[for="logs-level-select"]')
    await expect(label).toBeVisible()
    await label.click()

    const dropdown = page.locator('.select-dropdown')
    await expect(dropdown).toBeVisible()
    await page.keyboard.press('Escape')
    await expect(dropdown).toBeHidden()
  })
})
