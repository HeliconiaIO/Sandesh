/**
 * The raw source viewer (§19).
 *
 * The raw bytes are authoritative (ADR-003), so this view must not improve
 * them. Line endings are split for display only — the underlying string is
 * never rewritten — and the byte offsets shown are computed from the actual
 * separators so they line up with the MIME tree's ranges.
 *
 * ## The signature interaction (§18)
 *
 * Selecting a MIME part passes its `body_offset`/`body_length` here, and the
 * lines covering that range are highlighted and scrolled into view. This is
 * only possible because parts are stored as ranges into the raw bytes rather
 * than as copies, so the highlight is exact rather than a re-parse that might
 * disagree.
 */

import { useEffect, useMemo, useRef } from 'react'

import { CopyButton } from '../../components/CopyButton'
import './RawViewer.css'

/** A line, with the byte offset at which it starts. */
interface Line {
  number: number
  offset: number
  text: string
}

/**
 * Splits into lines while tracking byte offsets.
 *
 * Offsets are measured in UTF-8 bytes because the backend's ranges are byte
 * ranges; using string indices would drift on any non-ASCII header.
 */
function toLines(raw: string): Line[] {
  const encoder = new TextEncoder()
  const lines: Line[] = []
  let offset = 0
  let number = 1
  let cursor = 0

  while (cursor <= raw.length) {
    const index = raw.indexOf('\n', cursor)
    const end = index === -1 ? raw.length : index
    const text = raw.slice(cursor, end).replace(/\r$/, '')

    lines.push({ number, offset, text })

    if (index === -1) break

    // The consumed span includes the separator exactly as it appeared.
    offset += encoder.encode(raw.slice(cursor, index + 1)).length
    cursor = index + 1
    number += 1
  }

  return lines
}

interface RawViewerProps {
  raw: string
  /** The byte range to highlight, from the selected MIME part. */
  highlight?: { offset: number; length: number } | null
}

export function RawViewer({ raw, highlight }: RawViewerProps) {
  const lines = useMemo(() => toLines(raw), [raw])
  const containerRef = useRef<HTMLDivElement>(null)

  const range = highlight
    ? { start: highlight.offset, end: highlight.offset + highlight.length }
    : null

  useEffect(() => {
    if (!range || !containerRef.current) return
    const first = containerRef.current.querySelector('[data-highlighted="true"]')
    // Scrolling is a convenience; where it is unavailable the highlight itself
    // must still render correctly.
    if (first instanceof HTMLElement && typeof first.scrollIntoView === 'function') {
      first.scrollIntoView({ block: 'center', behavior: 'smooth' })
    }
  }, [highlight?.offset, highlight?.length])

  return (
    <div className="raw">
      <div className="raw__toolbar">
        <span className="raw__meta">
          {lines.length} lines · {new TextEncoder().encode(raw).length} bytes
        </span>
        {range ? (
          <span className="raw__meta raw__meta--accent">
            Showing bytes {range.start}–{range.end}
          </span>
        ) : null}
        <CopyButton value={raw} label="the raw source" />
      </div>

      <div className="raw__body" ref={containerRef}>
        <pre className="raw__pre">
          {lines.map((line) => {
            const end = line.offset + new TextEncoder().encode(line.text).length
            const highlighted = range !== null && end >= range.start && line.offset <= range.end

            return (
              <div
                key={line.number}
                className={`raw__line${highlighted ? ' raw__line--highlight' : ''}`}
                data-highlighted={highlighted ? 'true' : undefined}
              >
                <span className="raw__number" aria-hidden="true">
                  {line.number}
                </span>
                <span className="raw__text">{line.text || ' '}</span>
              </div>
            )
          })}
        </pre>
      </div>
    </div>
  )
}
