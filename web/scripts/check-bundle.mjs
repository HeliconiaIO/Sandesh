/**
 * Entry-bundle guard.
 *
 * The composer's editor stack (TipTap + ProseMirror) is about a megabyte of
 * source. It used to sit in the entry chunk because `ComposerProvider` wraps
 * every route and mounted the composer unconditionally, so every session that
 * only ever *read* mail still downloaded a rich-text editor.
 *
 * It is split out now, but that split is one static `import` away from
 * silently reverting — and the symptom is invisible in review and in tests:
 * everything still works, the first paint just costs twice as much. So it is
 * asserted here instead.
 *
 * Run after `npm run build`.
 */

import { readFileSync, readdirSync, existsSync } from 'node:fs'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'

const DIST = fileURLToPath(new URL('../dist/assets', import.meta.url))

/** Raw (pre-gzip) ceiling for the entry chunk. */
const ENTRY_LIMIT_KB = 550

/**
 * Packages that must never appear in the entry chunk. Matched against the
 * entry's own sourcemap, which names every module rolled into it.
 */
const MUST_BE_SPLIT = [
  '@tiptap',
  'prosemirror-view',
  'prosemirror-model',
  'prosemirror-state',
  'linkifyjs',
]

if (!existsSync(DIST)) {
  console.error('No dist/assets — run `npm run build` first.')
  process.exit(1)
}

const files = readdirSync(DIST)
const entry = files.find((f) => f.startsWith('index-') && f.endsWith('.js'))

if (!entry) {
  console.error('No entry chunk (index-*.js) in dist/assets.')
  process.exit(1)
}

const problems = []

const entryKb = readFileSync(join(DIST, entry)).length / 1024
if (entryKb > ENTRY_LIMIT_KB) {
  problems.push(
    `Entry chunk is ${entryKb.toFixed(0)} kB, over the ${ENTRY_LIMIT_KB} kB budget.`,
  )
}

const mapPath = join(DIST, `${entry}.map`)
if (existsSync(mapPath)) {
  const sources = JSON.parse(readFileSync(mapPath, 'utf8')).sources ?? []
  for (const pkg of MUST_BE_SPLIT) {
    const hit = sources.find((s) => s.includes(`node_modules/${pkg}`))
    if (hit) {
      problems.push(
        `${pkg} is in the entry chunk — it must stay in a lazy chunk.\n` +
          `    first module: ${hit}`,
      )
    }
  }
} else {
  // Not fatal: the size check above still applies.
  console.warn(`No sourcemap beside ${entry}; checked size only.`)
}

if (problems.length > 0) {
  console.error('\nBundle regression:\n')
  for (const p of problems) console.error(`  - ${p}`)
  console.error(
    '\nSomething is probably imported statically that should be lazy.\n' +
      'The composer is loaded via React.lazy in ComposerContext.tsx; a plain\n' +
      '`import` of MessageComposer (or of TipTap directly) anywhere in the\n' +
      'entry graph pulls the whole editor back in.\n',
  )
  process.exit(1)
}

console.log(
  `Bundle OK — entry ${entryKb.toFixed(0)} kB (budget ${ENTRY_LIMIT_KB} kB), ` +
    `editor stack stays split.`,
)
