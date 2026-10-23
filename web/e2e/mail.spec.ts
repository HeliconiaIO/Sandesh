/**
 * The Phase 4 acceptance test (§105, §106, §107).
 *
 * The flow that must work end to end:
 *
 * ```text
 * SMTP → core → storage → events → WebSocket → UI
 * ```
 *
 * Nothing is seeded into the database (§67): every message here is delivered
 * over a real SMTP conversation, so a break anywhere in the pipe fails the
 * test rather than being papered over by a fixture.
 */

import { expect, test } from '@playwright/test'

import { fetchSubjects } from './imap'
import { sendMail } from './smtp'

/** A subject unique to this run, so assertions cannot match an older message. */
const stamp = () => `${Date.now()}-${Math.floor(Math.random() * 10_000)}`

test.describe('Sandesh', () => {
  test('shows the connection details on first load', async ({ page }) => {
    // The onboarding question — "how do I point my app at this?" — must be
    // answered by the first screen (§60).
    await page.goto('/')

    await expect(page.getByRole('heading', { name: 'Send mail here' })).toBeVisible()
    // The address appears in the header and in the panel; assert the panel's.
    // The port follows `playwright.config.ts`, so a run on an overridden port
    // asserts the address that run actually listens on rather than a literal.
    const smtpPort = process.env.SANDESH_E2E_SMTP_PORT ?? '1025'
    await expect(page.locator('#main').getByText(`127.0.0.1:${smtpPort}`)).toBeVisible()
  })

  test('reports a live event stream', async ({ page }) => {
    await page.goto('/')
    // The banner's indicator, not the Overview's status tile.
    await expect(page.getByRole('banner').getByText('Live', { exact: true })).toBeVisible()
  })

  test('a message sent over SMTP appears without a refresh', async ({ page }) => {
    // The headline Phase 4 feature. The page is loaded BEFORE the message is
    // sent and is never reloaded, so the only way the message can appear is
    // through the WebSocket.
    const subject = `Live delivery ${stamp()}`

    // Open the mailbox the mail will land in, so what is asserted is live
    // insertion into a list the user is already looking at.
    await page.goto('/')
    await page.getByRole('link', { name: /INBOX/ }).first().click()
    await expect(page.getByRole('banner').getByText('Live', { exact: true })).toBeVisible()

    // Wait until the list has finished loading before delivering. Sending into
    // a still-loading view is a race the user never runs: the fetch would
    // return the message anyway, so the test would pass without the event
    // stream doing anything, which is the opposite of what it must prove.
    //
    // An empty mailbox settles on its empty state rather than on a listbox, so
    // both are accepted. Waiting only for the listbox passed locally — where
    // the mailbox always had mail — and failed on a clean CI database, which is
    // the state a first run actually starts from.
    await expect(
      page
        .getByRole('listbox', { name: 'Messages' })
        .or(page.getByText('No messages yet')),
    ).toBeVisible()

    await sendMail({
      from: 'sender@example.test',
      to: 'dev@example.test',
      data: [
        'From: sender@example.test',
        'To: dev@example.test',
        `Subject: ${subject}`,
        '',
        'Delivered while the page was open.',
      ].join('\n'),
    })

    // The new message is in the list, with no navigation or reload. Asserted
    // by subject rather than by row count: the list is capped at one page, so
    // a full mailbox cannot grow and a count would never change.
    const row = page.getByRole('option').filter({ hasText: subject })
    await expect(row).toHaveCount(1, { timeout: 15_000 })

    // Newest first, so live mail lands at the top where it is actually seen,
    // and is genuinely reachable rather than merely present in the DOM.
    await expect(page.getByRole('option').first()).toContainText(subject)
    await row.scrollIntoViewIfNeeded()
    await expect(row).toBeVisible()
  })

  test('opens a message and shows its content, headers, MIME and source', async ({ page }) => {
    const subject = `Inspect me ${stamp()}`

    await sendMail({
      from: 'inspector@example.test',
      to: 'dev@example.test',
      data: [
        'From: inspector@example.test',
        'To: dev@example.test',
        `Subject: ${subject}`,
        'X-Custom-Header: sandesh-e2e',
        'Content-Type: text/plain; charset=utf-8',
        '',
        'The body of the inspected message.',
      ].join('\n'),
    })

    await page.goto('/')
    await page.getByRole('link', { name: /INBOX/ }).first().click()

    await page.getByRole('option').filter({ hasText: subject }).click()

    // Preview: the body, and the sender.
    await expect(page.getByRole('heading', { name: subject })).toBeVisible()
    await expect(page.getByText('inspector@example.test').first()).toBeVisible()
    await expect(page.getByText('The body of the inspected message.')).toBeVisible()

    // Headers: every header, including the custom one — not a curated subset.
    await page.getByRole('tab', { name: 'Headers' }).click()
    await expect(
      page.getByRole('rowheader', { name: 'X-Custom-Header' }),
    ).toBeVisible()
    await expect(page.getByRole('cell', { name: 'sandesh-e2e' })).toBeVisible()

    // MIME: the structure, from the backend's parser.
    await page.getByRole('tab', { name: 'MIME' }).click()
    await expect(page.getByText('text/plain').first()).toBeVisible()

    // Raw: the authoritative bytes.
    await page.getByRole('tab', { name: 'Raw' }).click()
    await expect(page.getByText(`Subject: ${subject}`).first()).toBeVisible()
    await expect(page.getByText('X-Custom-Header: sandesh-e2e').first()).toBeVisible()
  })

  test('links a MIME part to its bytes in the raw source', async ({ page }) => {
    // The signature interaction (§18): selecting a part highlights exactly the
    // range the backend reported, which is only possible because parts are
    // byte ranges into the raw message rather than copies.
    const subject = `Multipart ${stamp()}`

    await sendMail({
      from: 'multipart@example.test',
      to: 'dev@example.test',
      data: [
        'From: multipart@example.test',
        'To: dev@example.test',
        `Subject: ${subject}`,
        'MIME-Version: 1.0',
        'Content-Type: multipart/alternative; boundary="frontier"',
        '',
        '--frontier',
        'Content-Type: text/plain',
        '',
        'The plain text alternative.',
        '--frontier',
        'Content-Type: text/html',
        '',
        '<p>The HTML alternative.</p>',
        '--frontier--',
      ].join('\n'),
    })

    await page.goto('/')
    await page.getByRole('link', { name: /INBOX/ }).first().click()
    await page.getByRole('option').filter({ hasText: subject }).click()
    await page.getByRole('tab', { name: 'MIME' }).click()

    // The tree reflects the real structure. Scoped to the tree, because the
    // same content type also appears in the raw source pane beside it.
    const tree = page.getByRole('tree', { name: 'MIME structure' })
    await expect(tree.getByText('multipart/alternative')).toBeVisible()

    await tree.getByText('text/html').click()

    // Selecting the part reveals its byte range and highlights it in the
    // source pane beside the tree.
    await expect(page.getByText('Byte range')).toBeVisible()
    await expect(page.getByText(/Showing bytes \d+–\d+/)).toBeVisible()
    await expect(page.locator('[data-highlighted="true"]').first()).toBeVisible()
  })

  test('renders HTML mail without letting it run script', async ({ page }) => {
    // §107: a message that tries to execute JavaScript, reach the parent window
    // and touch the application DOM must fail at all three.
    const subject = `Hostile HTML ${stamp()}`
    const marker = `pwned-${stamp()}`

    await sendMail({
      from: 'attacker@example.test',
      to: 'dev@example.test',
      data: [
        'From: attacker@example.test',
        'To: dev@example.test',
        `Subject: ${subject}`,
        'MIME-Version: 1.0',
        'Content-Type: text/html; charset=utf-8',
        '',
        '<h1>Rendered heading</h1>',
        `<script>`,
        `  document.title = '${marker}';`,
        `  try { window.parent.document.body.setAttribute('data-${marker}', '1') } catch (e) {}`,
        `  try { window.parent.localStorage.setItem('${marker}', '1') } catch (e) {}`,
        `</script>`,
        `<img src=x onerror="window.parent.document.title='${marker}'">`,
      ].join('\n'),
    })

    await page.goto('/')
    await page.getByRole('link', { name: /INBOX/ }).first().click()
    await page.getByRole('option').filter({ hasText: subject }).click()

    // The message renders...
    const frame = page.frameLocator('iframe[title="Message body"]')
    await expect(frame.getByRole('heading', { name: 'Rendered heading' })).toBeVisible()

    // ...but nothing it attempted took effect in the application's origin.
    await expect(page).not.toHaveTitle(new RegExp(marker))
    expect(await page.locator(`body[data-${marker}]`).count()).toBe(0)
    expect(await page.evaluate((key) => window.localStorage.getItem(key), marker)).toBeNull()

    // And the sandbox that guarantees it is still fully closed.
    expect(await page.locator('iframe[title="Message body"]').getAttribute('sandbox')).toBe('')
  })

  test('shows an attachment without executing it', async ({ page }) => {
    // §106: text, HTML and a binary attachment in one message.
    const subject = `With attachment ${stamp()}`

    await sendMail({
      from: 'sender@example.test',
      to: 'dev@example.test',
      data: [
        'From: sender@example.test',
        'To: dev@example.test',
        `Subject: ${subject}`,
        'MIME-Version: 1.0',
        'Content-Type: multipart/mixed; boundary="outer"',
        '',
        '--outer',
        'Content-Type: text/plain',
        '',
        'See the attachment.',
        '--outer',
        'Content-Type: application/octet-stream; name="report.bin"',
        'Content-Disposition: attachment; filename="report.bin"',
        'Content-Transfer-Encoding: base64',
        '',
        'U2FuZGVzaCBQaGFzZSA0',
        '--outer--',
      ].join('\n'),
    })

    await page.goto('/')
    await page.getByRole('link', { name: /INBOX/ }).first().click()
    await page.getByRole('option').filter({ hasText: subject }).click()

    await page.getByRole('tab', { name: /Attachments/ }).click()

    await expect(page.getByText('report.bin')).toBeVisible()
    await expect(page.getByText('application/octet-stream')).toBeVisible()

    // The only offered actions are inspect and download — never inline render.
    await expect(page.getByRole('link', { name: /Download/ })).toBeVisible()
  })

  test('downloads an attachment whose bytes match the API', async ({ page }) => {
    // The previous test proves the attachment is *offered* without being
    // rendered. This one proves the offer is real: the download is taken, and
    // the bytes are compared against the API's own copy.
    //
    // Comparing the two is what makes this worth a browser test. Asserting a
    // size, or that a file simply arrived, would pass for a truncated or
    // re-encoded body — and the transfer encoding is exactly where an
    // attachment gets corrupted. `U2FuZGVzaCBQaGFzZSA0` is base64 in the
    // message, so a server that forgot to decode it would still deliver a
    // plausible-looking file.
    const subject = `Download me ${stamp()}`

    await sendMail({
      from: 'sender@example.test',
      to: 'dev@example.test',
      data: [
        'From: sender@example.test',
        'To: dev@example.test',
        `Subject: ${subject}`,
        'MIME-Version: 1.0',
        'Content-Type: multipart/mixed; boundary="outer"',
        '',
        '--outer',
        'Content-Type: text/plain',
        '',
        'See the attachment.',
        '--outer',
        'Content-Type: application/octet-stream; name="report.bin"',
        'Content-Disposition: attachment; filename="report.bin"',
        'Content-Transfer-Encoding: base64',
        '',
        'U2FuZGVzaCBQaGFzZSA0',
        '--outer--',
      ].join('\n'),
    })

    await page.goto('/')
    await page.getByRole('link', { name: /INBOX/ }).first().click()
    await page.getByRole('option').filter({ hasText: subject }).click()
    await page.getByRole('tab', { name: /Attachments/ }).click()

    const link = page.getByRole('link', { name: /Download/ })
    await expect(link).toBeVisible()

    // Take the download the user would take, rather than reading the href and
    // fetching it directly: the anchor carries `download`, and following it is
    // the behaviour under test.
    const [download] = await Promise.all([page.waitForEvent('download'), link.click()])
    expect(download.suggestedFilename()).toBe('report.bin')

    const stream = await download.createReadStream()
    const chunks: Buffer[] = []
    for await (const chunk of stream) chunks.push(Buffer.from(chunk))
    const downloaded = Buffer.concat(chunks)

    // The API's copy of the same bytes, fetched through the page so the request
    // carries whatever session the browser has.
    const href = await link.getAttribute('href')
    const viaApi = Buffer.from(
      await page.evaluate(async (url) => {
        const response = await fetch(url)
        return [...new Uint8Array(await response.arrayBuffer())]
      }, href!),
    )

    expect(downloaded.equals(viaApi)).toBe(true)
    // Pinned to the decoded content, so the pair cannot agree on the wrong
    // thing: two identically-broken reads would satisfy the comparison above.
    expect(downloaded.toString('utf8')).toBe('Sandesh Phase 4')
  })

  test('search narrows the list to matching messages', async ({ page }) => {
    // Two messages that share a word and one that does not, so the assertion
    // can be that search *excludes* as well as includes. A query matching
    // everything present would pass against a search that does nothing at all.
    const run = stamp()
    const needle = `Needle-${run}`
    const haystack = `Haystack-${run}`

    for (const [subject, from] of [
      [`${needle} alpha`, 'alice@example.test'],
      [`${haystack} beta`, 'bob@example.test'],
      [`${haystack} gamma`, 'carol@example.test'],
    ]) {
      await sendMail({
        from,
        to: 'dev@example.test',
        data: [
          `From: ${from}`,
          'To: dev@example.test',
          `Subject: ${subject}`,
          '',
          'Body.',
        ].join('\n'),
      })
    }

    await page.goto('/')
    await page.getByRole('link', { name: /INBOX/ }).first().click()
    // Wait for all three to land before searching, so a miss is a search
    // failure rather than a message that had not arrived yet.
    await expect(page.getByRole('option').filter({ hasText: run })).toHaveCount(3, {
      timeout: 15_000,
    })

    await page.getByRole('link', { name: 'Search', exact: true }).click()
    const input = page.getByRole('searchbox', { name: 'Search messages' })
    await input.fill(haystack)
    await input.press('Enter')

    const results = page.getByRole('option')
    await expect(results.filter({ hasText: haystack })).toHaveCount(2, { timeout: 15_000 })
    // The one that must be absent. This is the half that fails if the query is
    // ignored and the full mailbox is returned.
    await expect(results.filter({ hasText: needle })).toHaveCount(0)

    // A field query, not just full text: `from:` is parsed rather than matched
    // as a literal, which a substring search over the subject would fail.
    await input.fill('from:carol@example.test')
    await input.press('Enter')
    await expect(results.filter({ hasText: `${haystack} gamma` })).toHaveCount(1, {
      timeout: 15_000,
    })
    await expect(results.filter({ hasText: `${haystack} beta` })).toHaveCount(0)
  })

  test('keeps the selected message across a reload', async ({ page }) => {
    // §13/§58: navigation state lives in the URL, so a refresh returns the
    // user to where they were rather than to an empty pane.
    const subject = `Deep link ${stamp()}`

    await sendMail({
      from: 'sender@example.test',
      to: 'dev@example.test',
      data: [
        'From: sender@example.test',
        'To: dev@example.test',
        `Subject: ${subject}`,
        '',
        'Body.',
      ].join('\n'),
    })

    await page.goto('/')
    await page.getByRole('link', { name: /INBOX/ }).first().click()
    await page.getByRole('option').filter({ hasText: subject }).click()

    await expect(page).toHaveURL(/\/mailbox\/[^/]+\/message\/[^/]+/)

    await page.reload()

    await expect(page.getByRole('heading', { name: subject })).toBeVisible()
  })

  test('marks a message read through the API', async ({ page }) => {
    const subject = `Flag me ${stamp()}`

    await sendMail({
      from: 'sender@example.test',
      to: 'dev@example.test',
      data: [
        'From: sender@example.test',
        'To: dev@example.test',
        `Subject: ${subject}`,
        '',
        'Body.',
      ].join('\n'),
    })

    await page.goto('/')
    await page.getByRole('link', { name: /INBOX/ }).first().click()
    await page.getByRole('option').filter({ hasText: subject }).click()

    // Opening a message from the list marks it read, as a mail client does, so
    // the viewer already offers the inverse action. Asserting "Mark read" here
    // would be asserting that the list does *not* do what it is meant to.
    await expect(page.getByRole('button', { name: 'Mark unread' })).toBeVisible()

    // Drive the flag both ways. Each label change is the server's answer coming
    // back through the query cache, not local state: a mutation that never
    // reached the API would leave the control where it was.
    await page.getByRole('button', { name: 'Mark unread' }).click()
    await expect(page.getByRole('button', { name: 'Mark read' })).toBeVisible()

    await page.getByRole('button', { name: 'Mark read' }).click()
    await expect(page.getByRole('button', { name: 'Mark unread' })).toBeVisible()

    // The flag is persisted, not merely rendered: a reload re-fetches the
    // message and must still find it read.
    await page.reload()
    await expect(page.getByRole('button', { name: 'Mark unread' })).toBeVisible()
  })

  test('deletes a message from the list and from IMAP', async ({ page }) => {
    // The one listed scenario that crosses a protocol boundary: the delete is
    // performed in the UI, and the result is read back over IMAP. Checking the
    // REST API instead would only show that it agrees with the call the UI just
    // made — the mail client is the surface that can disagree.
    //
    // From INBOX, `Delete` moves to Trash rather than purging (the viewer
    // offers `Delete permanently` only once an entry is already in Trash), so
    // the assertion is that the message *moved*: gone from INBOX, present in
    // Trash. Asserting only its absence would pass just as well if the delete
    // had destroyed mail it was supposed to keep.
    const subject = `Delete me ${stamp()}`

    await sendMail({
      from: 'sender@example.test',
      to: 'dev@example.test',
      data: [
        'From: sender@example.test',
        'To: dev@example.test',
        `Subject: ${subject}`,
        '',
        'This message is about to be deleted.',
      ].join('\n'),
    })

    await page.goto('/')
    await page.getByRole('link', { name: /INBOX/ }).first().click()

    const row = page.getByRole('option').filter({ hasText: subject })
    await expect(row).toHaveCount(1, { timeout: 15_000 })

    // IMAP sees it before the delete, so the assertion afterwards is a change
    // this test caused rather than a mailbox that never held the message.
    expect(await fetchSubjects({ mailbox: 'INBOX' })).toContain(subject)

    await row.click()
    await expect(page.getByRole('heading', { name: subject })).toBeVisible()
    await page.getByRole('button', { name: 'Delete', exact: true }).click()

    // The row goes without a reload: the viewer closes on success and the list
    // is invalidated, so a delete that only emptied the viewer would still be
    // caught here.
    await expect(row).toHaveCount(0, { timeout: 15_000 })

    // The protocol agrees with the UI. This is the half that a REST assertion
    // cannot reach: the IMAP layer keeps its own per-mailbox UID and flag state,
    // so a message can vanish from one view while a mail client still lists it.
    expect(await fetchSubjects({ mailbox: 'INBOX' })).not.toContain(subject)
    expect(await fetchSubjects({ mailbox: 'Trash' })).toContain(subject)
  })
})
