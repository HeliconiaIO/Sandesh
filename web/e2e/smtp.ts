/**
 * A minimal SMTP client for the browser tests.
 *
 * Hand-rolled rather than pulled from npm: the suite needs to send a handful of
 * fixed messages, and a dependency would add a maintenance surface for
 * something that is thirty lines of protocol.
 */

import { Socket } from 'node:net'

export async function sendMail(options: {
  host?: string
  port?: number
  from: string
  to: string
  /** The complete message, headers and body, using CRLF. */
  data: string
}): Promise<void> {
  const host = options.host ?? '127.0.0.1'
  // Matches `playwright.config.ts`, which binds the server to the same
  // overridable port; otherwise a run on a non-default port would start a
  // server the mail helper could not reach.
  const port = options.port ?? Number(process.env.SANDESH_E2E_SMTP_PORT ?? 1025)

  const socket = new Socket()
  socket.setEncoding('utf8')

  const expect = (prefix: string) =>
    new Promise<void>((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error(`timed out waiting for ${prefix}`)), 10_000)

      const onData = (chunk: string) => {
        // A multiline reply repeats the code with `-`; only the final line,
        // which uses a space, ends the exchange.
        const done = chunk
          .split(/\r?\n/)
          .filter(Boolean)
          .some((line) => line.startsWith(`${prefix} `))
        if (!done) return

        clearTimeout(timer)
        socket.off('data', onData)
        if (chunk.startsWith(prefix)) resolve()
        else reject(new Error(`expected ${prefix}, got: ${chunk}`))
      }

      socket.on('data', onData)
    })

  await new Promise<void>((resolve, reject) => {
    socket.once('error', reject)
    socket.connect(port, host, resolve)
  })

  await expect('220')

  const say = async (command: string, code: string) => {
    socket.write(`${command}\r\n`)
    await expect(code)
  }

  await say('EHLO e2e.test', '250')
  await say(`MAIL FROM:<${options.from}>`, '250')
  await say(`RCPT TO:<${options.to}>`, '250')
  await say('DATA', '354')

  // Dot-stuffing: a line that is a lone dot would otherwise end the message.
  const body = options.data.replace(/\r?\n/g, '\r\n').replace(/^\./gm, '..')
  socket.write(`${body}\r\n.\r\n`)
  await expect('250')

  socket.write('QUIT\r\n')
  socket.end()
}
