/**
 * The MIME inspector (§17).
 *
 * The tree is reconstructed from the flat `parts` array the backend returns,
 * using each part's `children` paths. There is deliberately no MIME parsing
 * here (§18): `mail-parser` in `sandesh-core` is the only parser in the
 * product, and a second implementation in TypeScript would eventually disagree
 * with it — at which point the tool would be lying about the message.
 */

import type { MimePart, MimeTree as MimeTreeData } from '../../api/client'
import { formatSize } from '../../lib/format'
import './MimeTree.css'

interface MimeTreeProps {
  tree: MimeTreeData
  selectedPath: string | null
  onSelect: (path: string) => void
}

export function MimeTree({ tree, selectedPath, onSelect }: MimeTreeProps) {
  const byPath = new Map(tree.parts.map((part) => [part.path, part]))
  const root = tree.parts.find((part) => part.path === '')

  if (!root) {
    return <p className="mime__empty">This message has no MIME structure.</p>
  }

  return (
    <div className="mime">
      <ul className="mime__tree" role="tree" aria-label="MIME structure">
        <MimeNode
          part={root}
          byPath={byPath}
          depth={0}
          selectedPath={selectedPath}
          onSelect={onSelect}
        />
      </ul>
    </div>
  )
}

function MimeNode({
  part,
  byPath,
  depth,
  selectedPath,
  onSelect,
}: {
  part: MimePart
  byPath: Map<string, MimePart>
  depth: number
  selectedPath: string | null
  onSelect: (path: string) => void
}) {
  const children = part.children
    .map((path) => byPath.get(path))
    .filter((child): child is MimePart => Boolean(child))

  const selected = selectedPath === part.path
  const multipart = part.content_type.startsWith('multipart/')

  return (
    <li role="none" className="mime__item">
      <div
        role="treeitem"
        aria-selected={selected}
        aria-expanded={children.length > 0 ? true : undefined}
        aria-level={depth + 1}
        tabIndex={selected ? 0 : -1}
        className={`mime__node${selected ? ' mime__node--selected' : ''}`}
        style={{ paddingLeft: `${depth * 14 + 8}px` }}
        onClick={() => onSelect(part.path)}
        onKeyDown={(event) => {
          if (event.key === 'Enter' || event.key === ' ') {
            event.preventDefault()
            onSelect(part.path)
          }
        }}
      >
        <code className="mime__type">{part.content_type}</code>
        {part.filename ? <span className="mime__filename">{part.filename}</span> : null}
        {part.is_attachment ? <span className="chip">attachment</span> : null}
        {/* Multipart nodes carry no bytes of their own worth sizing. */}
        {!multipart ? (
          <span className="mime__size">{formatSize(part.decoded_size ?? part.body_length)}</span>
        ) : null}
      </div>

      {children.length > 0 ? (
        <ul role="group" className="mime__group">
          {children.map((child) => (
            <MimeNode
              key={child.path}
              part={child}
              byPath={byPath}
              depth={depth + 1}
              selectedPath={selectedPath}
              onSelect={onSelect}
            />
          ))}
        </ul>
      ) : null}
    </li>
  )
}

/** The metadata panel for the selected part. */
export function MimeDetail({ part }: { part: MimePart }) {
  const rows: Array<[string, string]> = [
    ['Path', part.path === '' ? 'root' : part.path],
    ['Content type', part.content_type],
    ['Encoding', part.encoding],
    ['Charset', part.charset ?? '—'],
    ['Filename', part.filename ?? '—'],
    ['Disposition', part.is_attachment ? 'attachment' : 'inline'],
    // The byte range is the link to the raw source, and the reason the MIME
    // and Raw tabs can agree exactly (§18).
    ['Byte range', `${part.body_offset}–${part.body_offset + part.body_length}`],
    ['Raw length', formatSize(part.body_length)],
    [
      'Decoded size',
      part.decoded_size == null ? '—' : formatSize(part.decoded_size),
    ],
  ]

  return (
    <dl className="mime__detail">
      {rows.map(([label, value]) => (
        <div key={label} className="mime__row">
          <dt>{label}</dt>
          <dd className="mono">{value}</dd>
        </div>
      ))}
    </dl>
  )
}
