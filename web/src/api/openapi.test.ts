/**
 * The OpenAPI reading the console depends on (Phase 7 A5).
 *
 * These assert against the *real* generated document wherever the question is
 * "does this hold for our contract" rather than "does this function work",
 * because a console that handles a hypothetical document and not ours is no
 * use. `docs/openapi.json` is the same file the server serves and CI pins
 * against drift.
 */

import { describe, expect, it } from 'vitest'

import document from '../../../docs/openapi.json'
import {
  buildPath,
  buildQuery,
  describe as describeText,
  flatten,
  resolve,
  sampleFor,
  typeLabel,
  type OpenApiDocument,
} from './openapi'

// `as unknown` first: the imported JSON is inferred far more narrowly
// than the partial shape this module reads.
const contract = document as unknown as OpenApiDocument

describe('flatten', () => {
  it('finds every operation in the real contract', () => {
    const operations = flatten(contract)

    // Counted independently of `flatten` so the test cannot agree with a bug
    // in the function it is checking.
    const expected = Object.values(contract.paths ?? {}).reduce(
      (total, methods) =>
        total +
        Object.keys(methods).filter((key) =>
          ['get', 'post', 'put', 'patch', 'delete'].includes(key),
        ).length,
      0,
    )

    expect(operations).toHaveLength(expected)
    expect(operations.length).toBeGreaterThan(30)
  })

  it('gives every operation a unique id, so selection is unambiguous', () => {
    const operations = flatten(contract)
    const ids = new Set(operations.map((operation) => operation.id))

    expect(ids.size).toBe(operations.length)
  })

  it('orders operations stably regardless of document key order', () => {
    const once = flatten(contract).map((operation) => operation.id)
    const reversed: OpenApiDocument = {
      ...contract,
      paths: Object.fromEntries(Object.entries(contract.paths ?? {}).reverse()),
    }

    expect(flatten(reversed).map((operation) => operation.id)).toEqual(once)
  })

  it('assigns every operation a tag, so none is unreachable in a grouped list', () => {
    for (const operation of flatten(contract)) {
      expect(operation.tag).not.toBe('')
    }
  })
})

describe('buildPath', () => {
  it('substitutes a supplied parameter', () => {
    expect(buildPath('/api/v1/accounts/{id}', { id: 'abc' })).toBe('/api/v1/accounts/abc')
  })

  /**
   * A blank parameter must not collapse the segment. `/accounts//entries`
   * returns a 404 that looks like the endpoint is missing, where the visible
   * `{id}` shows what the request is still waiting for.
   */
  it('leaves a blank parameter visible rather than collapsing the segment', () => {
    expect(buildPath('/api/v1/accounts/{id}', {})).toBe('/api/v1/accounts/{id}')
    expect(buildPath('/api/v1/accounts/{id}', { id: '   ' })).toBe('/api/v1/accounts/{id}')
  })

  it('encodes a value that would otherwise change the path', () => {
    expect(buildPath('/api/v1/messages/{id}', { id: 'a/../b' })).toBe(
      '/api/v1/messages/a%2F..%2Fb',
    )
  })

  it('substitutes every parameter when a path has more than one', () => {
    expect(buildPath('/x/{a}/y/{b}', { a: '1', b: '2' })).toBe('/x/1/y/2')
  })
})

describe('buildQuery', () => {
  it('omits blank values rather than sending empty parameters', () => {
    expect(buildQuery({ limit: '10', cursor: '' }, ['limit', 'cursor'])).toBe('?limit=10')
  })

  it('returns nothing when no parameter has a value', () => {
    expect(buildQuery({}, ['limit'])).toBe('')
  })

  it('encodes values', () => {
    expect(buildQuery({ q: 'a b&c' }, ['q'])).toBe('?q=a+b%26c')
  })
})

describe('resolve', () => {
  it('follows a $ref into components', () => {
    const resolved = resolve({ $ref: '#/components/schemas/AccountDto' }, contract)

    expect(resolved?.type).toBe('object')
  })

  it('returns undefined for a $ref that does not exist', () => {
    expect(resolve({ $ref: '#/components/schemas/Nope' }, contract)).toBeUndefined()
  })

  /** A cycle must terminate rather than exhaust the stack. */
  it('stops on a self-referential schema', () => {
    const cyclic: OpenApiDocument = {
      components: {
        schemas: {
          Node: { $ref: '#/components/schemas/Node' },
        },
      },
    }

    expect(resolve({ $ref: '#/components/schemas/Node' }, cyclic)).toBeUndefined()
  })
})

describe('sampleFor', () => {
  it('produces JSON that parses for every request body in the contract', () => {
    let checked = 0

    for (const methods of Object.values(contract.paths ?? {})) {
      for (const operation of Object.values(methods)) {
        const schema = operation.requestBody?.content?.['application/json']?.schema
        if (!schema) continue

        const sample = sampleFor(schema, contract)
        // The editor is seeded with this text; if it cannot round-trip, the
        // console opens on a body the user must fix before doing anything.
        expect(() => JSON.parse(JSON.stringify(sample))).not.toThrow()
        checked += 1
      }
    }

    expect(checked).toBeGreaterThan(0)
  })

  it('types primitives so the seeded body only needs its values changed', () => {
    const schema = {
      type: 'object',
      properties: {
        name: { type: 'string' },
        count: { type: 'integer' },
        enabled: { type: 'boolean' },
      },
    }

    expect(sampleFor(schema, contract)).toEqual({ name: '', count: 0, enabled: false })
  })

  it('prefers a declared default over an invented value', () => {
    expect(sampleFor({ type: 'integer', default: 50 }, contract)).toBe(50)
  })

  /** A nullable string seeds an empty string, not `null`: the field is
      editable text, and `null` makes the user delete it before typing. */
  it('seeds a null union from its real type', () => {
    expect(sampleFor({ type: ['string', 'null'] }, contract)).toBe('')
    expect(sampleFor({ type: ['integer', 'null'] }, contract)).toBe(0)
  })

  it('terminates on a cyclic schema rather than recursing forever', () => {
    const cyclic: OpenApiDocument = {
      components: {
        schemas: {
          Node: {
            type: 'object',
            properties: { child: { $ref: '#/components/schemas/Node' } },
          },
        },
      },
    }

    expect(() => sampleFor({ $ref: '#/components/schemas/Node' }, cyclic)).not.toThrow()
  })
})

describe('typeLabel', () => {
  it('labels arrays by their item type', () => {
    expect(typeLabel({ type: 'array', items: { type: 'string' } }, contract)).toBe('string[]')
  })

  it('lists enum members, which are the values a field actually accepts', () => {
    expect(typeLabel({ type: 'string', enum: ['smtp', 'imap'] }, contract)).toBe('smtp | imap')
  })

  it('falls back to any for an unknown schema rather than guessing', () => {
    expect(typeLabel(undefined, contract)).toBe('any')
  })

  /**
   * OpenAPI 3.1 is JSON Schema 2020-12, where a nullable string is
   * `type: ["string", "null"]`. 63 schemas in this contract use that form.
   * Reading `type` as a bare string labels every one of them `any`.
   */
  it('collapses a null union to its real type', () => {
    expect(typeLabel({ type: ['string', 'null'] }, contract)).toBe('string?')
    expect(typeLabel({ type: ['integer', 'null'] }, contract)).toBe('integer?')
  })

  it('marks a nullable field so it is distinguishable from a required one', () => {
    expect(typeLabel({ type: 'string' }, contract)).toBe('string')
    expect(typeLabel({ type: 'string', nullable: true }, contract)).toBe('string?')
  })

  it('never labels a real contract field as any', () => {
    // Every property of every schema must resolve to something nameable; an
    // `any` here means the console is failing to read its own contract.
    for (const schema of Object.values(contract.components?.schemas ?? {})) {
      for (const property of Object.values(schema.properties ?? {})) {
        expect(typeLabel(property, contract)).not.toBe('any')
      }
    }
  })
})

describe('describe', () => {
  /**
   * The contract's descriptions are Rust doc comments, so `# Errors` and
   * backticked codes reach the UI verbatim. Rendered raw they look like a
   * defect; this is the minimum that makes them read correctly.
   */
  it('turns a markdown heading into a heading segment', () => {
    const lines = describeText('# Errors\n`404` if absent.')

    expect(lines[0]).toEqual([{ kind: 'heading', text: 'Errors' }])
  })

  it('splits inline code out of the surrounding prose', () => {
    const lines = describeText('`400` for an unparseable id.')

    expect(lines[0]).toEqual([
      { kind: 'code', text: '400' },
      { kind: 'text', text: ' for an unparseable id.' },
    ])
  })

  it('drops blank lines rather than rendering empty paragraphs', () => {
    expect(describeText('one\n\n\ntwo')).toHaveLength(2)
  })

  it('leaves plain prose untouched', () => {
    expect(describeText('Lists messages.')).toEqual([
      [{ kind: 'text', text: 'Lists messages.' }],
    ])
  })

  it('handles every description in the real contract without throwing', () => {
    for (const methods of Object.values(contract.paths ?? {})) {
      for (const operation of Object.values(methods)) {
        expect(() => describeText(operation.description ?? '')).not.toThrow()
      }
    }
  })
})
