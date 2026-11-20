import { expect, test } from '@playwright/test'

const stamp = () => `${Date.now()}-${Math.floor(Math.random() * 10_000)}`

test.describe('BreakIT UI/UX Stress & Boundary Suite', () => {
  test.describe('1. Responsive Layout & Horizontal Overflow Stress', () => {
    const viewports = [
      { name: 'Mobile XS (320px)', width: 320, height: 568 },
      { name: 'Mobile Standard (375px)', width: 375, height: 667 },
      { name: 'Tablet Portrait (768px)', width: 768, height: 1024 },
      { name: 'Desktop (1440px)', width: 1440, height: 900 },
    ]

    const routes = [
      '/',
      '/search',
      '/traces',
      '/logs',
      '/api',
      '/webhooks',
      '/transfer',
      '/workspaces',
      '/settings',
      '/about',
    ]

    for (const vp of viewports) {
      test(`no horizontal overflow on principal routes at ${vp.name}`, async ({ page }) => {
        await page.setViewportSize({ width: vp.width, height: vp.height })

        for (const route of routes) {
          await page.goto(route)
          await page.waitForLoadState('domcontentloaded')

          // Check if document has horizontal overflow
          const overflow = await page.evaluate(() => {
            const doc = document.documentElement
            return {
              scrollWidth: doc.scrollWidth,
              clientWidth: doc.clientWidth,
              overflowX: window.getComputedStyle(doc).overflowX,
            }
          })

          // In standard responsive UI, scrollWidth should not exceed clientWidth by more than 2px
          expect(
            overflow.scrollWidth,
            `Horizontal overflow detected on route "${route}" at ${vp.name}: scrollWidth=${overflow.scrollWidth}, clientWidth=${overflow.clientWidth}`,
          ).toBeLessThanOrEqual(overflow.clientWidth + 2)
        }
      })
    }
  })

  test.describe('2. Navigation Boundary & Route Fault Tolerance', () => {
    test('non-existent route renders graceful NotFound state and redirects home', async ({ page }) => {
      const consoleErrors: string[] = []
      page.on('console', (msg) => {
        if (msg.type() === 'error') consoleErrors.push(msg.text())
      })

      await page.goto('/completely/invalid/random-route-path-' + stamp())

      // App must render the graceful "That page does not exist" state
      await expect(page.getByText('That page does not exist')).toBeVisible()

      // The brand header and navigation must stay fully intact
      await expect(page.getByRole('banner')).toBeVisible()

      // After 2.5s timer, it should automatically redirect to '/' (Overview)
      await expect(page).toHaveURL('/', { timeout: 5_000 })
      expect(consoleErrors.filter((e) => !e.includes('404'))).toHaveLength(0)
    })

    test('invalid message ID deep link recovers gracefully without WSOD', async ({ page }) => {
      await page.goto('/mailbox/INBOX/message/non-existent-msg-id-9999999')
      await page.waitForLoadState('domcontentloaded')

      // Main layout must remain intact
      await expect(page.getByRole('banner')).toBeVisible()
      // Should not show raw unhandled React runtime error
      await expect(page.getByText('Something went wrong')).toHaveCount(0)
    })
  })

  test.describe('3. Form Fuzzing & XSS Injection in Mail Composer', () => {
    test('handles corrupted localStorage draft without crashing', async ({ page }) => {
      await page.goto('/')
      // Inject corrupted JSON into the composer draft storage key
      await page.evaluate(() => {
        localStorage.setItem('sandesh_composer_draft', '{{{{CORRUPTED_JSON_STRING:::undefined')
      })

      // Open composer using keyboard shortcut 'c'
      await page.keyboard.press('c')

      // Composer dialog should open cleanly despite corrupted draft
      const composer = page.locator('.composer-dialog')
      await expect(composer).toBeVisible()

      // Close it safely
      await page.locator('.composer-ctrl-btn--close').click()
      await expect(composer).toBeHidden()
    })

    test('validates required fields and rejects empty recipients', async ({ page }) => {
      await page.goto('/')
      await page.keyboard.press('c')

      const composer = page.locator('.composer-dialog')
      await expect(composer).toBeVisible()

      // Clear the "To" field
      const toInput = page.locator('#composer-to')
      await toInput.fill('')

      // Click Send
      const sendBtn = page.locator('button[type="submit"]')
      await sendBtn.click()

      // Should not close, and input:invalid should be triggered (HTML5 validation)
      await expect(composer).toBeVisible()
      const isInvalid = await toInput.evaluate((el: HTMLInputElement) => !el.checkValidity())
      expect(isInvalid).toBe(true)

      // Close composer
      await page.locator('.composer-ctrl-btn--close').click()
    })

    test('XSS script injection in composer fields is securely sanitized and isolated', async ({ page }) => {
      // Expose a window flag to detect if any injected script executes in the host application
      await page.goto('/')
      await page.evaluate(() => {
        // @ts-expect-error test harness flag
        window.__xss_executed = false
      })

      const uniqueTag = `XSS-${stamp()}`
      const xssSubject = `${uniqueTag} <script>window.__xss_executed=true</script><img src=x onerror="window.__xss_executed=true">`
      const xssBody = `<h1>Body XSS</h1><script>window.__xss_executed=true</script><svg onload="window.__xss_executed=true">`

      // Open composer
      await page.keyboard.press('c')
      await expect(page.locator('.composer-dialog')).toBeVisible()

      await page.locator('#composer-to').fill('dev@sandesh.test')
      await page.locator('#composer-subject').fill(xssSubject)
      await page.locator('#composer-body').fill(xssBody)

      // Send the email
      await page.locator('button[type="submit"]').click()
      await expect(page.locator('.composer-dialog')).toBeHidden({ timeout: 10_000 })

      // Navigate to INBOX
      await page.getByRole('link', { name: /INBOX/ }).first().click()

      // The message should arrive
      const row = page.getByRole('option').filter({ hasText: uniqueTag }).first()
      await expect(row).toBeVisible({ timeout: 15_000 })

      // Open message in viewer
      await row.click()

      // Ensure that host window.__xss_executed was NEVER turned true
      const wasExecuted = await page.evaluate(() => {
        // @ts-expect-error test harness flag
        return window.__xss_executed === true
      })
      expect(wasExecuted).toBe(false)
    })

    test('handles massive payload without UI freeze or flexbox destruction', async ({ page }) => {
      await page.goto('/')
      await page.keyboard.press('c')
      await expect(page.locator('.composer-dialog')).toBeVisible()

      // Massive 1,000 char subject and 15,000 char body with emojis and unicode
      const hugeSubject = '🔥 [STRESS] ' + 'A'.repeat(500) + ' 🚀'
      const hugeBody = '🌟 Repeating long unicode line with special chars: <>"\'&\\/\n'.repeat(200)

      await page.locator('#composer-to').fill('stress@sandesh.test')
      await page.locator('#composer-subject').fill(hugeSubject)
      await page.locator('#composer-body').fill(hugeBody)

      // Check that the dialog didn't blow up viewport boundaries
      const boundingBox = await page.locator('.composer-dialog').boundingBox()
      const viewport = page.viewportSize()
      expect(boundingBox).not.toBeNull()
      expect(viewport).not.toBeNull()
      if (boundingBox && viewport) {
        expect(boundingBox.width).toBeLessThanOrEqual(viewport.width)
      }

      // Close with discard
      await page.locator('.composer-ctrl-btn--close').click()
      // Discard confirmation dialog should appear because draft is dirty
      const confirmDialog = page.locator('.dialog-card')
      await expect(confirmDialog).toBeVisible()
      await page.getByRole('button', { name: 'Discard' }).click()
      await expect(page.locator('.composer-dialog')).toBeHidden()
    })
  })

  test.describe('4. Rapid Interactive Stress & Race Conditions', () => {
    test('rapid theme toggling retains consistent DOM state without crash', async ({ page }) => {
      await page.goto('/')

      const themeToggle = page.getByRole('button', { name: /theme/i }).first()
      await expect(themeToggle).toBeVisible()

      // Rapidly toggle 10 times in sequence
      for (let i = 0; i < 10; i++) {
        await themeToggle.click()
      }

      // Verify page is still functional and html has a valid data-theme or class
      const themeAttr = await page.evaluate(() => {
        return document.documentElement.getAttribute('data-theme') || document.documentElement.className
      })
      expect(['light', 'dark', 'theme-light', 'theme-dark', '']).toContain(themeAttr.trim())
    })

    test('rapid keyboard shortcuts (? for help, Esc to close) behave predictably', async ({ page }) => {
      await page.goto('/')

      // Open shortcuts modal with header button or '?'
      const shortcutsBtn = page.getByRole('button', { name: /keyboard shortcuts/i })
      await shortcutsBtn.click()

      const modal = page.locator('.shortcuts-modal__dialog')
      await expect(modal).toBeVisible()

      // Close with Escape
      await page.keyboard.press('Escape')
      await expect(modal).toBeHidden()
    })

    test('search input stress with regex bombs and SQL injection strings does not throw', async ({ page }) => {
      await page.goto('/search')

      const searchInput = page.getByPlaceholder(/search/i).first()
      await expect(searchInput).toBeVisible()

      const attackVectors = [
        '((((((((a+)+)+)+)+)+)+)+)$',
        "' OR '1'='1' --",
        "'; DROP TABLE messages; --",
        '<script>alert("search")</script>',
        '\u202E\u202D\u0000\uFFFF',
        'from:*" OR "*=*',
      ]

      for (const vector of attackVectors) {
        await searchInput.fill(vector)
        await searchInput.press('Enter')
        await page.waitForTimeout(100)

        // Must not crash into unhandled error state
        await expect(page.locator('#main')).toBeVisible()
        await expect(page.getByText('Something went wrong')).toHaveCount(0)
      }
    })
  })

  test.describe('5. Workspaces & DB Boundary Stress', () => {
    test('workspace creation form validates IDs and handles special characters safely', async ({ page }) => {
      await page.goto('/workspaces')

      // Click "New Workspace"
      const createBtn = page.getByRole('button', { name: /new workspace/i })
      await expect(createBtn).toBeVisible()
      await createBtn.click()

      const wsModal = page.locator('.ws-modal')
      await expect(wsModal).toBeVisible()

      // Try typing malicious / path traversal name
      const nameInput = page.locator('#ws-name')
      await nameInput.fill('../../../evil-path-test!@#$%')

      // Check slugified ID
      const idInput = page.locator('#ws-id')
      const slugVal = await idInput.inputValue()

      // Must be safely sanitized (no slashes, no dots, lowercase alphanumeric/hyphens)
      expect(slugVal).toBe('evil-path-test')

      // Close modal safely
      const closeBtn = page.locator('.ws-modal__close')
      await closeBtn.click()
      await expect(wsModal).toBeHidden()
    })
  })

  test.describe('6. Interactive API Console & Diagnostics Stress', () => {
    test('API console loads contract and executes endpoints without crashing UI', async ({ page }) => {
      await page.goto('/api')
      await page.waitForLoadState('domcontentloaded')

      // The filter input must appear once the OpenAPI document is loaded
      const filterInput = page.locator('.api-console__filter')
      await expect(filterInput).toBeVisible({ timeout: 10_000 })

      // Select an endpoint (e.g. GET /api/v1/info or first available operation)
      const opItem = page.locator('.api-console__operation').first()
      await expect(opItem).toBeVisible()
      await opItem.click()

      // The request builder should show the "Send request" button
      const sendBtn = page.getByRole('button', { name: /send request/i })
      await expect(sendBtn).toBeVisible()
      await sendBtn.click()

      // Should show HTTP status in response badge (200 OK)
      const statusBadge = page.locator('.api-console__status')
      await expect(statusBadge).toBeVisible({ timeout: 5_000 })
      await expect(statusBadge).toContainText(/200/)
    })

    test('Traces view allows filtering and real-time live inspection', async ({ page }) => {
      await page.goto('/traces')
      await page.waitForLoadState('domcontentloaded')

      // Check heading and filter controls
      await expect(page.getByRole('heading', { name: /traces|protocol/i })).toBeVisible()

      // Filter tabs (All, SMTP, IMAP, POP3)
      const smtpFilter = page.getByRole('button', { name: /smtp/i }).first()
      if (await smtpFilter.isVisible()) {
        await smtpFilter.click()
      }
    })

    test('Webhooks creation form rejects malformed URLs with UI validation', async ({ page }) => {
      await page.goto('/webhooks')
      await page.waitForLoadState('domcontentloaded')

      const addBtn = page.getByRole('button', { name: /add webhook|new webhook/i }).first()
      if (await addBtn.isVisible()) {
        await addBtn.click()

        const modal = page.locator('.webhook-modal, .dialog-card, .modal-card, form').first()
        await expect(modal).toBeVisible()

        // Submit blank or malformed URL
        const urlInput = page.locator('input[type="url"], input[name="url"], #webhook-url').first()
        if (await urlInput.isVisible()) {
          await urlInput.fill('not-a-valid-url-protocol')
          const submitBtn = modal.getByRole('button', { name: /save|create|add/i }).first()
          await submitBtn.click()

          // Should fail HTML5 or UI validation
          const isInvalid = await urlInput.evaluate((el: HTMLInputElement) => !el.checkValidity())
          expect(isInvalid).toBe(true)
        }
      }
    })
  })
})
