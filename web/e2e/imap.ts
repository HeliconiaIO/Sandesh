/**
 * A minimal IMAP client for the browser tests.
 *
 * Hand-rolled for the same reason as `smtp.ts`: the suite needs to ask one
 * question — which subjects does a mailbox hold? — and a dependency would add a
 * maintenance surface for something that is a short protocol exchange.
 *
 * This exists so a deletion made in the UI can be checked against the protocol
 * a real mail client speaks, rather than against the REST API the UI just
 * called. Asserting the same surface that performed the action would prove only
 * that the API agrees with itself.
 */

import { Socket } from 'node:net'

/**
 * The subjects in `mailbox`, in whatever order the server returns them.
 *
 * Subjects rather than counts: a count cannot distinguish "the message under
 * test went away" from "some other message did", and every test here stamps its
 * subject to be unique across runs.
 */
export async function fetchSubjects(options: {
  host?: string
  port?: number
  mailbox: string
  user?: string
  password?: string
}): Promise<string[]> {
  const host = options.host ?? '127.0.0.1'
  // Mirrors `smtp.ts`: the port is overridable so a run on non-default ports
  // reaches the server that run actually started.
  const port = options.port ?? Number(process.env.SANDESH_E2E_IMAP_PORT ?? 1143)
  // Authentication is off by default (ADR-005), so any credentials are
  // accepted; these are the ones the integration guide documents.
  const user = options.user ?? 'catch-all'
  const password = options.password ?? 'any'

  const socket = new Socket()
  socket.setEncoding('utf8')

  let buffer = ''
  const pending: Array<() => void> = []
  socket.on('data', (chunk: string) => {
    buffer += chunk
    // Wake every waiter: each re-tests its own completion condition against the
    // whole buffer, so a reply split across packets settles on a later chunk.
    for (const wake of pending.splice(0)) wake()
  })

  /** Waits until `done` is satisfied by the accumulated buffer. */
  const until = (done: (text: string) => boolean, what: string) =>
    new Promise<void>((resolve, reject) => {
      const timer = setTimeout(
        () => reject(new Error(`timed out waiting for ${what}; got: ${buffer}`)),
        10_000,
      )
      const check = () => {
        if (!done(buffer)) {
          pending.push(check)
          return
        }
        clearTimeout(timer)
        resolve()
      }
      check()
    })

  await new Promise<void>((resolve, reject) => {
    socket.once('error', reject)
    socket.connect(port, host, resolve)
  })

  await until((t) => t.includes('* OK'), 'the greeting')

  let counter = 0
  /** Sends one tagged command and resolves with everything it returned. */
  const send = async (command: string): Promise<string> => {
    const tag = `a${++counter}`
    buffer = ''
    socket.write(`${tag} ${command}\r\n`)
    // A tagged reply ends the command; NO and BAD end it too, and are surfaced
    // as errors rather than being read as an empty result.
    await until(
      (t) => new RegExp(`^${tag} (OK|NO|BAD)`, 'm').test(t),
      `a reply to ${command.split(' ')[0]}`,
    )
    const reply = buffer
    const status = new RegExp(`^${tag} (OK|NO|BAD)`, 'm').exec(reply)?.[1]
    if (status !== 'OK') throw new Error(`${command.split(' ')[0]} failed: ${reply}`)
    return reply
  }

  try {
    await send(`LOGIN ${user} ${password}`)
    await send(`SELECT "${options.mailbox}"`)

    // `1:*` on an empty mailbox is an error in some servers and an empty result
    // in others, so the count decides whether to fetch at all.
    const search = await send('SEARCH ALL')
    const ids = (/^\* SEARCH([^\r\n]*)/m.exec(search)?.[1] ?? '').trim()
    if (ids === '') return []

    const fetched = await send(`FETCH ${ids.split(/\s+/).join(',')} (BODY.PEEK[HEADER.FIELDS (SUBJECT)])`)
    return [...fetched.matchAll(/^Subject:\s*(.+?)\s*$/gim)].map((m) => m[1])
  } finally {
    socket.write('a999 LOGOUT\r\n')
    socket.end()
  }
}
