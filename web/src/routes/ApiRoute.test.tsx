/**
 * The API reference and console (Phase 7 A5).
 *
 * The criterion is "the API console can execute every documented endpoint",
 * and the two halves are tested separately here: the list is *derived* from
 * the document (so it cannot fall behind the server), and a selected operation
 * really issues the request a developer expects — right method, right URL,
 * right headers.
 *
 * The token assertions are the ones worth keeping honest. A console that
 * quietly persists a bearer token, or sends it somewhere unexpected, is a
 * credential leak in a tool people point at real mail.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { fireEvent, screen, waitFor } from '@testing-library/react'

import { renderWithProviders } from '../test/render'
import { ApiRoute } from './ApiRoute'

const DOCUMENT = {
  openapi: '3.1.0',
  info: { title: 'Sandesh', version: '0.1.0' },
  paths: {
    '/api/v1/messages': {
      get: {
        tags: ['messages'],
        summary: '`GET /api/v1/messages`.',
        description: 'Lists messages.',
        parameters: [
          { name: 'limit', in: 'query', description: 'Page size', schema: { type: 'integer' } },
        ],
        responses: { '200': { description: 'A page of messages.' } },
      },
    },
    '/api/v1/accounts/{id}': {
      get: {
        tags: ['accounts'],
        summary: '`GET /api/v1/accounts/{id}`.',
        parameters: [
          { name: 'id', in: 'path', required: true, schema: { type: 'string' } },
        ],
        responses: { '404': { description: 'No such account.' } },
      },
      delete: {
        tags: ['accounts'],
        summary: '`DELETE /api/v1/accounts/{id}`.',
        parameters: [
          { name: 'id', in: 'path', required: true, schema: { type: 'string' } },
        ],
        responses: { '204': { description: 'Deleted.' } },
      },
    },
    '/api/v1/send': {
      post: {
        tags: ['messages'],
        summary: '`POST /api/v1/send`.',
        requestBody: {
          required: true,
          content: {
            'application/json': {
              schema: {
                type: 'object',
                properties: { subject: { type: 'string' }, to: { type: 'array', items: { type: 'string' } } },
              },
            },
          },
        },
        responses: { '202': { description: 'Accepted.' } },
      },
    },
  },
  components: { schemas: {} },
}

/** An empty page, the shape every list endpoint returns. */
const EMPTY_PAGE = '{"items":[],"next_cursor":null}'

/**
 * Responds to the document fetch, then defers to `then` for API calls.
 *
 * The fallback returns an empty page rather than an arbitrary object because
 * the shared providers mount the composer, which fetches accounts on render;
 * a body it cannot read crashes the tree before the console is even visible.
 */
function mockFetch(then?: (url: string, init?: RequestInit) => Response) {
  return vi.fn((input: unknown, init?: RequestInit) => {
    const url = String(input)

    if (url === '/api/openapi.json') {
      return Promise.resolve(
        new Response(JSON.stringify(DOCUMENT), {
          status: 200,
          headers: { 'content-type': 'application/json' },
        }),
      )
    }

    // Only requests the console itself issues reach `then`. The composer
    // mounted by the shared providers fetches exactly `/api/v1/accounts`, and
    // treating that as a console request would both crash the composer and
    // record a call no test made.
    const fromConsole = url.startsWith('/api/v1/') && url !== '/api/v1/accounts'

    return Promise.resolve(
      (fromConsole ? then?.(url, init) : undefined) ??
        new Response(EMPTY_PAGE, {
          status: 200,
          headers: { 'content-type': 'application/json' },
        }),
    )
  })
}

beforeEach(() => {
  window.localStorage.clear()
})

afterEach(() => {
  vi.unstubAllGlobals()
})

describe('the operation list', () => {
  it('lists every operation the document declares', async () => {
    vi.stubGlobal('fetch', mockFetch())
    renderWithProviders(<ApiRoute />)

    // Four operations across three paths, derived rather than hard-coded in
    // the component: the count is what proves the list follows the contract.
    await waitFor(() => expect(screen.getByText('4 of 4')).toBeInTheDocument())

    expect(screen.getAllByText('/api/v1/accounts/{id}')).toHaveLength(2)
    expect(screen.getByText('/api/v1/send')).toBeInTheDocument()
  })

  it('filters by path, method and tag', async () => {
    vi.stubGlobal('fetch', mockFetch())
    renderWithProviders(<ApiRoute />)
    await waitFor(() => expect(screen.getByText('4 of 4')).toBeInTheDocument())

    fireEvent.change(screen.getByLabelText('Filter endpoints'), { target: { value: 'accounts' } })
    expect(screen.getByText('2 of 4')).toBeInTheDocument()

    fireEvent.change(screen.getByLabelText('Filter endpoints'), { target: { value: 'delete' } })
    expect(screen.getByText('1 of 4')).toBeInTheDocument()
  })

  it('explains itself when the contract cannot be loaded', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn((input: unknown) =>
        Promise.resolve(
          String(input) === '/api/openapi.json'
            ? new Response('nope', { status: 500 })
            : new Response(EMPTY_PAGE, {
                status: 200,
                headers: { 'content-type': 'application/json' },
              }),
        ),
      ),
    )
    renderWithProviders(<ApiRoute />)

    await waitFor(() =>
      expect(screen.getByText(/could not be loaded/i)).toBeInTheDocument(),
    )
  })
})

describe('executing a request', () => {
  it('sends the selected operation to its real path', async () => {
    const calls: Array<{ url: string; init: RequestInit | undefined }> = []
    vi.stubGlobal(
      'fetch',
      mockFetch((url, init) => {
        calls.push({ url, init })
        return new Response('{"items":[]}', {
          status: 200,
          statusText: 'OK',
          headers: { 'content-type': 'application/json' },
        })
      }),
    )

    renderWithProviders(<ApiRoute />)
    await waitFor(() => expect(screen.getByText('4 of 4')).toBeInTheDocument())

    fireEvent.click(screen.getByText('/api/v1/messages'))
    fireEvent.click(screen.getByRole('button', { name: 'Send request' }))

    await waitFor(() => expect(calls).toHaveLength(1))
    expect(calls[0]?.url).toBe('/api/v1/messages')
    expect(calls[0]?.init?.method).toBe('GET')
    await waitFor(() => expect(screen.getByText('200 OK')).toBeInTheDocument())
  })

  it('substitutes a path parameter and refuses to send until it has one', async () => {
    const calls: string[] = []
    vi.stubGlobal(
      'fetch',
      mockFetch((url) => {
        calls.push(url)
        return new Response('{}', { status: 200, headers: { 'content-type': 'application/json' } })
      }),
    )

    renderWithProviders(<ApiRoute />)
    await waitFor(() => expect(screen.getByText('4 of 4')).toBeInTheDocument())

    fireEvent.click(screen.getAllByText('/api/v1/accounts/{id}')[0]!)

    // Nothing may be sent while a required path parameter is missing: the
    // request would hit a different endpoint than the one selected.
    const send = screen.getByRole('button', { name: 'Send request' })
    expect(send).toBeDisabled()

    fireEvent.change(screen.getByPlaceholderText('id'), { target: { value: 'acct-1' } })
    expect(send).toBeEnabled()

    fireEvent.click(send)
    await waitFor(() => expect(calls).toEqual(['/api/v1/accounts/acct-1']))
  })

  it('appends only the query parameters that were filled in', async () => {
    const calls: string[] = []
    vi.stubGlobal(
      'fetch',
      mockFetch((url) => {
        calls.push(url)
        return new Response('{}', { status: 200, headers: { 'content-type': 'application/json' } })
      }),
    )

    renderWithProviders(<ApiRoute />)
    await waitFor(() => expect(screen.getByText('4 of 4')).toBeInTheDocument())

    fireEvent.click(screen.getByText('/api/v1/messages'))
    fireEvent.change(screen.getByPlaceholderText('Page size'), { target: { value: '5' } })
    fireEvent.click(screen.getByRole('button', { name: 'Send request' }))

    await waitFor(() => expect(calls).toEqual(['/api/v1/messages?limit=5']))
  })

  it('seeds a request body from the schema and sends it', async () => {
    let sent: RequestInit | undefined
    vi.stubGlobal(
      'fetch',
      mockFetch((_url, init) => {
        sent = init
        return new Response('{}', { status: 202, headers: { 'content-type': 'application/json' } })
      }),
    )

    renderWithProviders(<ApiRoute />)
    await waitFor(() => expect(screen.getByText('4 of 4')).toBeInTheDocument())

    fireEvent.click(screen.getByText('/api/v1/send'))

    const body = screen.getByLabelText('Request body') as HTMLTextAreaElement
    expect(JSON.parse(body.value)).toEqual({ subject: '', to: [''] })

    fireEvent.click(screen.getByRole('button', { name: 'Send request' }))

    await waitFor(() => expect(sent?.method).toBe('POST'))
    expect((sent?.headers as Record<string, string>)['Content-Type']).toBe('application/json')
  })

  /** A failed request is a real answer and must be shown, not swallowed. */
  it('shows an error status rather than hiding it', async () => {
    vi.stubGlobal(
      'fetch',
      mockFetch(
        () =>
          new Response('{"title":"Not Found"}', {
            status: 404,
            statusText: 'Not Found',
            headers: { 'content-type': 'application/json' },
          }),
      ),
    )

    renderWithProviders(<ApiRoute />)
    await waitFor(() => expect(screen.getByText('4 of 4')).toBeInTheDocument())

    fireEvent.click(screen.getByText('/api/v1/messages'))
    fireEvent.click(screen.getByRole('button', { name: 'Send request' }))

    await waitFor(() => expect(screen.getByText('404 Not Found')).toBeInTheDocument())
  })

  /**
   * A transport failure is not an HTTP status. Reporting it as one would
   * invent a response the server never sent.
   */
  it('distinguishes a network failure from a status code', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn((input: unknown) => {
        const url = String(input)
        if (url === '/api/openapi.json') {
          return Promise.resolve(
            new Response(JSON.stringify(DOCUMENT), {
              status: 200,
              headers: { 'content-type': 'application/json' },
            }),
          )
        }
        // The composer's own fetches must succeed, or the tree crashes
        // before the console can report the failure under test.
        if (url === '/api/v1/accounts') {
          return Promise.resolve(
            new Response(EMPTY_PAGE, {
              status: 200,
              headers: { 'content-type': 'application/json' },
            }),
          )
        }
        return Promise.reject(new Error('connection refused'))
      }),
    )

    renderWithProviders(<ApiRoute />)
    await waitFor(() => expect(screen.getByText('4 of 4')).toBeInTheDocument())

    fireEvent.click(screen.getByText('/api/v1/messages'))
    fireEvent.click(screen.getByRole('button', { name: 'Send request' }))

    await waitFor(() =>
      expect(screen.getByText(/never reached the server/i)).toBeInTheDocument(),
    )
  })
})

describe('the bearer token', () => {
  it('is sent as an Authorization header when supplied', async () => {
    let sent: RequestInit | undefined
    vi.stubGlobal(
      'fetch',
      mockFetch((_url, init) => {
        sent = init
        return new Response('{}', { status: 200, headers: { 'content-type': 'application/json' } })
      }),
    )

    renderWithProviders(<ApiRoute />)
    await waitFor(() => expect(screen.getByText('4 of 4')).toBeInTheDocument())

    fireEvent.click(screen.getByText('/api/v1/messages'))
    fireEvent.change(screen.getByPlaceholderText('(none)'), { target: { value: 'secret-token' } })
    fireEvent.click(screen.getByRole('button', { name: 'Send request' }))

    await waitFor(() => expect(sent).toBeDefined())
    expect((sent?.headers as Record<string, string>).Authorization).toBe('Bearer secret-token')
  })

  it('sends no Authorization header when none was entered', async () => {
    let sent: RequestInit | undefined
    vi.stubGlobal(
      'fetch',
      mockFetch((_url, init) => {
        sent = init
        return new Response('{}', { status: 200, headers: { 'content-type': 'application/json' } })
      }),
    )

    renderWithProviders(<ApiRoute />)
    await waitFor(() => expect(screen.getByText('4 of 4')).toBeInTheDocument())

    fireEvent.click(screen.getByText('/api/v1/messages'))
    fireEvent.click(screen.getByRole('button', { name: 'Send request' }))

    await waitFor(() => expect(sent).toBeDefined())
    expect((sent?.headers as Record<string, string>).Authorization).toBeUndefined()
  })

  /**
   * The decision behind A5: the UI shares an origin with the API, so a token
   * in browser storage is a credential at rest that anything on the origin can
   * read. This test fails if that is ever changed to persist.
   */
  it('is never written to browser storage', async () => {
    vi.stubGlobal('fetch', mockFetch())
    renderWithProviders(<ApiRoute />)
    await waitFor(() => expect(screen.getByText('4 of 4')).toBeInTheDocument())

    fireEvent.click(screen.getByText('/api/v1/messages'))
    fireEvent.change(screen.getByPlaceholderText('(none)'), { target: { value: 'secret-token' } })

    const storages = [window.localStorage, window.sessionStorage]
    for (const storage of storages) {
      for (let index = 0; index < storage.length; index += 1) {
        const key = storage.key(index)
        expect(storage.getItem(key ?? '')).not.toContain('secret-token')
      }
    }
  })

  it('is masked in the field, so it is not readable over a shoulder or a screen share', async () => {
    vi.stubGlobal('fetch', mockFetch())
    renderWithProviders(<ApiRoute />)
    await waitFor(() => expect(screen.getByText('4 of 4')).toBeInTheDocument())

    fireEvent.click(screen.getByText('/api/v1/messages'))
    expect(screen.getByPlaceholderText('(none)')).toHaveAttribute('type', 'password')
  })
})

describe('states (Phase 7 Pass B)', () => {
  /** A contract that failed to load is usually a server still starting. */
  it('offers a retry that reloads the contract', async () => {
    let attempts = 0
    vi.stubGlobal(
      'fetch',
      vi.fn((input: unknown) => {
        const url = String(input)
        if (url === '/api/openapi.json') {
          attempts += 1
          return attempts === 1
            ? Promise.resolve(new Response('nope', { status: 500 }))
            : Promise.resolve(
                new Response(JSON.stringify(DOCUMENT), {
                  status: 200,
                  headers: { 'content-type': 'application/json' },
                }),
              )
        }
        return Promise.resolve(
          new Response(EMPTY_PAGE, {
            status: 200,
            headers: { 'content-type': 'application/json' },
          }),
        )
      }),
    )

    renderWithProviders(<ApiRoute />)

    const retry = await screen.findByRole('button', { name: /try again/i })
    fireEvent.click(retry)

    await waitFor(() => expect(screen.getByText('4 of 4')).toBeInTheDocument())
    expect(attempts).toBeGreaterThan(1)
  })

  it('announces a load failure to assistive technology', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn((input: unknown) =>
        Promise.resolve(
          String(input) === '/api/openapi.json'
            ? new Response('nope', { status: 500 })
            : new Response(EMPTY_PAGE, {
                status: 200,
                headers: { 'content-type': 'application/json' },
              }),
        ),
      ),
    )

    renderWithProviders(<ApiRoute />)

    await waitFor(() => expect(screen.getByRole('alert')).toBeInTheDocument())
  })

  /** A filter matching nothing must say so rather than show a blank list. */
  it('says when no endpoint matches the filter', async () => {
    vi.stubGlobal('fetch', mockFetch())
    renderWithProviders(<ApiRoute />)
    await waitFor(() => expect(screen.getByText('4 of 4')).toBeInTheDocument())

    fireEvent.change(screen.getByLabelText('Filter endpoints'), {
      target: { value: 'zzzz-no-such-endpoint' },
    })

    expect(screen.getByText('0 of 4')).toBeInTheDocument()
    await waitFor(() => expect(screen.getByText(/No endpoint matches/i)).toBeInTheDocument())
  })
})
