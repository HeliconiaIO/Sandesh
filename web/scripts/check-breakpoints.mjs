/**
 * Breakpoint lint.
 *
 * The responsive contract (UI_UX_DIRECTION §4) is a table of four widths. It
 * had drifted to seventeen — 480, 560, 600, 640, 700, 720, 760, 768, 769, 820,
 * 840, 860, 880, 900, 960, 1000, 1024, 1100 — which is what made the layout
 * break between sizes: the shell collapsed to two panes at 1100px while the
 * route inside it still believed it had three until 1024px, so between those
 * widths the page held two contradictory layouts at once.
 *
 * That class of bug is invisible in review and only appears at one window
 * width, so it is asserted here instead. Every width in a CSS media query must
 * be one of the four documented values.
 */

import { readFileSync, readdirSync, statSync } from 'node:fs'
import { join, relative } from 'node:path'
import { fileURLToPath } from 'node:url'

const SRC = fileURLToPath(new URL('../src', import.meta.url))

/** The documented scale. See tokens.css, "Breakpoints". */
const ALLOWED = new Set([1280, 768, 640, 480])

/** Width-valued features. `resolution`/`aspect-ratio` etc. are not widths. */
const WIDTH_FEATURE = /\b(?:min|max)-width\s*:\s*([0-9.]+)(px|rem|em)/g

function* cssFiles(dir) {
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry)
    if (statSync(full).isDirectory()) yield* cssFiles(full)
    else if (entry.endsWith('.css')) yield full
  }
}

const violations = []

for (const file of cssFiles(SRC)) {
  const css = readFileSync(file, 'utf8')
  const lines = css.split('\n')

  lines.forEach((line, i) => {
    // Only media queries constrain layout; a container's own max-width is a
    // content measure and is deliberately unconstrained by this rule.
    if (!line.includes('@media')) return

    for (const [, value, unit] of line.matchAll(WIDTH_FEATURE)) {
      // rem/em in a media query resolve against the ROOT font size, which is
      // the browser default (16px) and not affected by our own type tokens.
      const px = unit === 'px' ? Number(value) : Number(value) * 16
      if (!ALLOWED.has(px)) {
        violations.push(
          `${relative(SRC, file)}:${i + 1}  ${px}px  ${line.trim()}`,
        )
      }
    }
  })
}

if (violations.length > 0) {
  console.error(
    `\nBreakpoint drift — ${violations.length} media quer${
      violations.length === 1 ? 'y uses a width' : 'ies use widths'
    } outside the documented scale.\n\n` +
      `Allowed (UI_UX_DIRECTION §4): ${[...ALLOWED]
        .sort((a, b) => b - a)
        .map((w) => `${w}px`)
        .join(', ')}\n`,
  )
  for (const v of violations) console.error(`  ${v}`)
  console.error(
    '\nPick the step that matches the intent: 1280 drops the detail pane, 768' +
      '\ndrops the list pane, 640 and 480 thin the chrome only.\n',
  )
  process.exit(1)
}

console.log(
  `Breakpoints OK — every media query uses the documented scale ` +
    `(${[...ALLOWED].sort((a, b) => b - a).join(' / ')}).`,
)
