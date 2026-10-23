import { fileURLToPath } from 'node:url'

import { defineConfig } from '@playwright/test'

/**
 * Where Sandesh keeps its data during the browser tests.
 *
 * Gitignored, so a run never writes a database next to the source and never
 * touches a developer's real mail. Sandesh creates the directory itself
 * (ADR-031), so nothing needs to exist beforehand.
 */
const SCRATCH = fileURLToPath(new URL('./.e2e/', import.meta.url))

/**
 * The ports the suite's own server binds.
 *
 * Overridable because 8080 is a popular port: a developer with something else
 * on it could not run this suite at all, and the failure ("Address already in
 * use") looks like a broken test rather than an occupied port. CI leaves these
 * unset and gets the defaults.
 */
const HTTP_PORT = process.env.SANDESH_E2E_HTTP_PORT ?? '8080'
const SMTP_PORT = process.env.SANDESH_E2E_SMTP_PORT ?? '1025'
// IMAP is bound explicitly for the same reason, and because the delete spec
// reads the result back over it: left unset, the server would take the default
// 1143 while HTTP and SMTP moved, so an overridden run would check a port this
// suite never started.
const IMAP_PORT = process.env.SANDESH_E2E_IMAP_PORT ?? '1143'
const BASE_URL = `http://127.0.0.1:${HTTP_PORT}`

/**
 * Browser tests (§66).
 *
 * These drive a real Sandesh binary: the UI is embedded, SMTP is real, and the
 * event stream is real. Nothing is stubbed, because the point of this suite is
 * to prove the whole pipe works — SMTP to core to storage to events to
 * WebSocket to UI.
 *
 * The server is started by the suite itself on the default ports so that a
 * developer can run `npm run e2e` without a second terminal.
 */
export default defineConfig({
  testDir: './e2e',
  // These tests share one server and one database, so they must not race.
  workers: 1,
  fullyParallel: false,
  timeout: 30_000,
  expect: { timeout: 10_000 },
  reporter: process.env.CI ? [['html', { open: 'never' }], ['list']] : 'list',
  use: {
    baseURL: BASE_URL,
    trace: 'retain-on-failure',
    /*
     * Pinned, and deliberately wider than 1280.
     *
     * Playwright's default is exactly 1280px, and `@media (max-width: 1280px)`
     * is inclusive, so the default viewport lands *inside* the breakpoint where
     * the sidebar becomes an off-canvas drawer. Every test that clicks a nav
     * link then fails with "element is outside of the viewport" — a property of
     * the harness, not of the app. Specs that want a narrow layout opt in with
     * their own `test.use({ viewport })`.
     */
    viewport: { width: 1440, height: 900 },
  },
  webServer: {
    // Release in CI (already built by the workflow), debug locally.
    command: process.env.CI
      ? './target/release/sandesh'
      : 'cargo run --quiet',
    url: `${BASE_URL}/api/v1/health`,
    reuseExistingServer: !process.env.CI,
    timeout: 180_000,
    // Run from the repository root so both commands resolve, and put the data
    // somewhere disposable by configuring it rather than by moving the process
    // (ADR-031). Before those variables existed this had to be done with `cwd`,
    // which moved every other relative path with it.
    cwd: fileURLToPath(new URL('..', import.meta.url)),
    env: {
      SANDESH_DATA_DIR: SCRATCH,
      SANDESH_HTTP_ADDR: `127.0.0.1:${HTTP_PORT}`,
      SANDESH_SMTP_ADDR: `127.0.0.1:${SMTP_PORT}`,
      SANDESH_IMAP_ADDR: `127.0.0.1:${IMAP_PORT}`,
    },
  },
})
