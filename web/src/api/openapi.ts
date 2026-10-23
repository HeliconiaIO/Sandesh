/**
 * The OpenAPI document, as the console needs to read it (Phase 7 A5).
 *
 * Fetched from `/api/openapi.json` on the running server rather than imported
 * from the repository copy. The two are kept identical by a CI drift gate, but
 * only one of them is the contract the server in front of you is actually
 * serving — and a console that documents a different build than it calls is
 * worse than no console, because it is confidently wrong.
 *
 * The types here are deliberately partial. This reads the parts an operation
 * list and a request builder need; it is not an OpenAPI validator, and
 * pretending otherwise would mean maintaining a schema model nothing reads.
 */

/** A JSON Schema fragment, to the depth the console renders.
 *
 * `type` may be an array: OpenAPI 3.1 is JSON Schema 2020-12, where a nullable
 * field is written `type: ["string", "null"]` rather than with a `nullable`
 * flag. 63 schemas in this contract use that form, so treating `type` as a
 * bare string would mislabel every optional field as `any` and seed it `null`.
 */
export interface SchemaNode {
  type?: string | string[]
  format?: string
  description?: string
  enum?: unknown[]
  default?: unknown
  items?: SchemaNode
  properties?: Record<string, SchemaNode>
  required?: string[]
  nullable?: boolean
  $ref?: string
  allOf?: SchemaNode[]
  oneOf?: SchemaNode[]
  anyOf?: SchemaNode[]
}

/** Where a parameter travels. */
export type ParameterLocation = 'path' | 'query' | 'header' | 'cookie'

export interface Parameter {
  name: string
  in: ParameterLocation
  description?: string
  required?: boolean
  schema?: SchemaNode
}

export interface RequestBody {
  description?: string
  required?: boolean
  content?: Record<string, { schema?: SchemaNode }>
}

export interface ResponseSpec {
  description?: string
  content?: Record<string, { schema?: SchemaNode }>
}

export interface RawOperation {
  tags?: string[]
  summary?: string
  description?: string
  operationId?: string
  parameters?: Parameter[]
  requestBody?: RequestBody
  responses?: Record<string, ResponseSpec>
}

export interface OpenApiDocument {
  openapi?: string
  info?: { title?: string; version?: string; description?: string }
  paths?: Record<string, Record<string, RawOperation>>
  components?: { schemas?: Record<string, SchemaNode> }
}

/** The HTTP methods an operation can use, in the order they are listed. */
export const METHODS = ['get', 'post', 'put', 'patch', 'delete'] as const

export type Method = (typeof METHODS)[number]

/** One operation, flattened so a list can render it without re-walking paths. */
export interface Operation {
  /** Stable identity for selection and for a test to address. */
  id: string
  path: string
  method: Method
  tag: string
  summary: string
  description: string
  parameters: Parameter[]
  requestBody?: RequestBody | undefined
  responses: Array<{ status: string; description: string }>
}

/**
 * Flattens the document into a list of operations.
 *
 * Sorted by tag then path so the list has a stable order regardless of the
 * document's key order, which is not guaranteed by JSON.
 */
export function flatten(document: OpenApiDocument): Operation[] {
  const operations: Operation[] = []

  for (const [path, methods] of Object.entries(document.paths ?? {})) {
    for (const method of METHODS) {
      const raw = methods[method]
      if (!raw) continue

      operations.push({
        id: `${method}:${path}`,
        path,
        method,
        tag: raw.tags?.[0] ?? 'other',
        // The generator writes the summary as the literal route, which is
        // already the heading; the description carries the real prose.
        summary: raw.summary?.trim() ?? '',
        description: raw.description?.trim() ?? '',
        parameters: raw.parameters ?? [],
        requestBody: raw.requestBody,
        responses: Object.entries(raw.responses ?? {})
          .map(([status, spec]) => ({
            status,
            description: spec?.description?.trim() ?? '',
          }))
          .sort((a, b) => a.status.localeCompare(b.status)),
      })
    }
  }

  return operations.sort(
    (a, b) => a.tag.localeCompare(b.tag) || a.path.localeCompare(b.path) || a.method.localeCompare(b.method),
  )
}

/** Resolves a local `$ref` against the document's component schemas. */
export function resolve(
  node: SchemaNode | undefined,
  document: OpenApiDocument,
  seen: Set<string> = new Set(),
): SchemaNode | undefined {
  if (!node) return undefined
  if (!node.$ref) return node

  // A self-referential schema (a tree node, say) would otherwise recurse
  // forever. Returning the unresolved node keeps the sample finite and honest.
  if (seen.has(node.$ref)) return undefined

  const name = node.$ref.replace('#/components/schemas/', '')
  const target = document.components?.schemas?.[name]
  if (!target) return undefined

  const next = new Set(seen)
  next.add(node.$ref)
  return resolve(target, document, next)
}

/**
 * Builds a placeholder value for a schema, for the request-body editor.
 *
 * A starting point a developer edits, not a valid request: the console cannot
 * know a real account id. Values are typed correctly so the JSON parses and
 * only the contents need changing.
 */
export function sampleFor(
  node: SchemaNode | undefined,
  document: OpenApiDocument,
  depth = 0,
): unknown {
  const schema = resolve(node, document)
  if (!schema || depth > 6) return null

  if (schema.default !== undefined) return schema.default
  if (schema.enum?.length) return schema.enum[0]

  // A nullable union is written as `oneOf` by the generator; the first
  // non-null branch is the one worth showing.
  const union = schema.oneOf ?? schema.anyOf ?? schema.allOf
  if (union?.length) {
    const branch = union.find((item) => resolve(item, document)?.type !== 'null') ?? union[0]
    return sampleFor(branch, document, depth + 1)
  }

  switch (primaryType(schema)) {
    case 'array':
      return [sampleFor(schema.items, document, depth + 1)].filter((item) => item !== null)
    case 'object': {
      const sample: Record<string, unknown> = {}
      for (const [key, value] of Object.entries(schema.properties ?? {})) {
        sample[key] = sampleFor(value, document, depth + 1)
      }
      return sample
    }
    case 'integer':
    case 'number':
      return 0
    case 'boolean':
      return false
    case 'string':
      return schema.format === 'date-time' ? new Date().toISOString() : ''
    default:
      return null
  }
}

/**
 * The meaningful type of a schema, with JSON Schema's `null` union collapsed.
 *
 * `["string", "null"]` describes an optional string; reporting the union
 * verbatim would make every optional field read as a different type from its
 * required twin.
 */
function primaryType(schema: SchemaNode): string | undefined {
  const { type } = schema
  if (!Array.isArray(type)) return type
  return type.find((member) => member !== 'null') ?? type[0]
}

/** Whether a schema permits null, in either spelling. */
function isNullable(schema: SchemaNode): boolean {
  return schema.nullable === true || (Array.isArray(schema.type) && schema.type.includes('null'))
}

/** A one-line type label for a parameter or property. */
export function typeLabel(node: SchemaNode | undefined, document: OpenApiDocument): string {
  const schema = resolve(node, document)
  if (!schema) return 'any'
  if (schema.enum?.length) return schema.enum.map((value) => String(value)).join(' | ')

  // A nullable *reference* is written as a `oneOf` of null and the schema,
  // not as a type union — the generator uses both spellings. Following it
  // here is what keeps `MessageSummary.from` from reading as `any`.
  const union = schema.oneOf ?? schema.anyOf ?? schema.allOf
  if (union?.length && !schema.type) {
    const branch = union.find((item) => {
      const resolved = resolve(item, document)
      return resolved && primaryType(resolved) !== 'null'
    })
    if (branch) {
      const nullable = union.some((item) => resolve(item, document)?.type === 'null')
      const label = typeLabel(branch, document)
      return nullable && !label.endsWith('?') ? `${label}?` : label
    }
  }

  const type = primaryType(schema)
  const base = type === 'array' ? `${typeLabel(schema.items, document)}[]` : (type ?? 'any')
  return isNullable(schema) ? `${base}?` : base
}

/**
 * Substitutes path parameters into a path template.
 *
 * A parameter left blank stays as its `{name}` placeholder rather than
 * collapsing to an empty segment: `/accounts//entries` would 404 with no
 * explanation, where the visible placeholder shows what is missing.
 */
export function buildPath(path: string, values: Record<string, string>): string {
  return path.replace(/\{([^}]+)\}/g, (placeholder, name: string) => {
    const value = values[name]?.trim()
    return value ? encodeURIComponent(value) : placeholder
  })
}

/** One piece of a rendered description. */
export interface DescriptionSegment {
  kind: 'heading' | 'text' | 'code'
  text: string
}

/**
 * Splits a description into renderable segments.
 *
 * The contract's descriptions come from Rust doc comments, so they carry
 * markdown — `# Errors` headings and backticked status codes. Rendered
 * verbatim those read as a defect ("# Errors" with a literal hash), and
 * pulling in a markdown library to fix it would add a dependency and a
 * sanitisation problem for two constructs.
 *
 * Deliberately handles only headings and inline code. Anything else is left
 * as text rather than half-rendered, which is the honest failure: unstyled
 * prose reads fine, a mangled table does not.
 */
export function describe(description: string): DescriptionSegment[][] {
  return description
    .split('\n')
    .map((line) => line.trim())
    .filter((line) => line.length > 0)
    .map((line) => {
      const heading = /^#{1,6}\s+(.*)$/.exec(line)
      if (heading) return [{ kind: 'heading' as const, text: heading[1] ?? '' }]

      // Split on backticked spans, keeping the delimiters' contents. The
      // capture group means odd indices are the code spans.
      return line
        .split(/`([^`]+)`/)
        .map((part, index) => ({
          kind: index % 2 === 1 ? ('code' as const) : ('text' as const),
          text: part,
        }))
        .filter((segment) => segment.text.length > 0)
    })
}

/** Appends the non-empty query parameters to a path. */
export function buildQuery(values: Record<string, string>, names: string[]): string {
  const query = new URLSearchParams()
  for (const name of names) {
    const value = values[name]?.trim()
    if (value) query.append(name, value)
  }
  const rendered = query.toString()
  return rendered ? `?${rendered}` : ''
}
