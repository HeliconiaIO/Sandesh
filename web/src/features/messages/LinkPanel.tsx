/**
 * Link and image extraction: where a message points (Phase 8 A6).
 *
 * The question this panel answers is the one a developer asks of a rendered
 * template they do not fully trust: *what did my mail actually reference?* The
 * Preview tab shows the message as a reader sees it, which is precisely where
 * a wrong link hides — the visible text says "Confirm your account" whatever
 * the `href` holds.
 *
 * ## Extracted, never fetched
 *
 * Nothing here has been resolved. The server enumerates URLs and makes no
 * network request, so this panel has no status codes to show and must not
 * imply it does. That is stated once in the caption and enforced by the
 * vocabulary throughout: hosts are "referenced", never "reachable"; an
 * `http://` link is marked "not encrypted", a fact about the URL, and never
 * scored as a risk. The panel reports what the message contains and leaves
 * every judgement to the reader — the same stance the header analysis takes
 * towards DKIM.
 *
 * ## Grouped by host, not listed flat
 *
 * A marketing message carries eighty links to four hosts. The flat list is
 * eighty rows that answer nothing; the grouping is four rows that answer the
 * question immediately, and expands when the individual URL matters.
 */

import { useMemo, useState } from 'react'
import { ChevronRight, Globe, Image, Link2, Paperclip, ShieldOff } from 'lucide-react'

import type { ExtractedLink, LinkReport } from '../../api/client'
import { CopyButton } from '../../components/CopyButton'
import './LinkPanel.css'

/** The host heading for links that have none: `mailto:`, `cid:`, relative. */
const NO_HOST = 'No host'

/** The kinds the server classifies into; anything else is a contract change. */
type Kind = 'link' | 'image' | 'resource'

const KIND_ICONS: Record<Kind, React.ReactNode> = {
  link: <Link2 size={12} aria-hidden="true" />,
  image: <Image size={12} aria-hidden="true" />,
  resource: <Paperclip size={12} aria-hidden="true" />,
}

const KIND_LABELS: Record<Kind, string> = {
  link: 'Link',
  image: 'Image',
  resource: 'Resource',
}

export function LinkPanel({ report }: { report: LinkReport }) {
  // Grouped once per report, not per render: an eighty-link message regroups
  // on every expand toggle otherwise.
  const groups = useMemo(() => groupByHost(report.links), [report.links])

  if (report.links.length === 0) {
    return (
      <p className="links__empty">
        This message's HTML references no links or images. A message with only a plain-text
        part has nothing to extract, which is not a defect.
      </p>
    )
  }

  return (
    <section className="links" aria-label="Links and images">
      <header className="links__summary">
        <Counter kind="link" count={report.link_count} />
        <Counter kind="image" count={report.image_count} />
        <Counter kind="resource" count={report.resource_count} />
        <span className="links__summary-hosts">
          <Globe size={12} aria-hidden="true" />
          {report.hosts.length} {report.hosts.length === 1 ? 'host' : 'hosts'}
        </span>
      </header>

      {/* Load-bearing, not fine print: it is the reason no row here carries a
          status, and the reason none ever will. */}
      <p className="links__caveat">{report.disclaimer}</p>

      <ul className="links__groups">
        {groups.map((group) => (
          <HostGroup key={group.host} host={group.host} links={group.links} />
        ))}
      </ul>
    </section>
  )
}

function Counter({ kind, count }: { kind: Kind; count: number }) {
  const noun = KIND_LABELS[kind].toLowerCase()
  return (
    <span className="links__counter" data-kind={kind}>
      {KIND_ICONS[kind]}
      {count} {count === 1 ? noun : `${noun}s`}
    </span>
  )
}

/**
 * Narrow the server's `kind` string to the union the icons are keyed by.
 *
 * An unrecognised kind falls back to `resource` rather than rendering nothing:
 * if the backend ever classifies something new, the URL must still appear — a
 * link silently missing from this panel is the one failure mode that matters.
 */
function asKind(kind: string): Kind {
  return kind === 'link' || kind === 'image' ? kind : 'resource'
}

function HostGroup({ host, links }: { host: string; links: ExtractedLink[] }) {
  // A single-link group is already fully described by its own row, so it opens
  // by default; a group of forty stays closed until asked for.
  const [open, setOpen] = useState(links.length <= 3)
  const insecure = links.some((link) => link.insecure)

  return (
    <li className="links__group">
      <button
        type="button"
        className="links__group-header"
        aria-expanded={open}
        onClick={() => setOpen((previous) => !previous)}
      >
        <ChevronRight
          size={13}
          aria-hidden="true"
          className="links__chevron"
          data-open={open}
        />
        <span className="links__host">{host}</span>
        {insecure ? (
          <span className="links__flag" title="Referenced over plain http://">
            <ShieldOff size={11} aria-hidden="true" />
            not encrypted
          </span>
        ) : null}
        <span className="links__group-count">{links.length}</span>
      </button>

      {open ? (
        <ul className="links__rows">
          {links.map((link, index) => (
            <LinkRow key={`${link.url}-${link.part_path}-${index}`} link={link} />
          ))}
        </ul>
      ) : null}
    </li>
  )
}

function LinkRow({ link }: { link: ExtractedLink }) {
  return (
    <li className="links__row">
      <span
        className="links__kind"
        data-kind={link.kind}
        title={KIND_LABELS[asKind(link.kind)]}
      >
        {KIND_ICONS[asKind(link.kind)]}
      </span>

      <div className="links__body">
        {/* The URL is text, never an anchor. Making it clickable here would
            put a one-click outbound request behind a panel whose entire
            premise is that Sandesh made none. Copy, then paste deliberately. */}
        <span className="links__url">{link.url}</span>

        <span className="links__meta">
          <code className="links__source">
            &lt;{link.element} {link.attribute}&gt;
          </code>
          {link.text ? <span className="links__text">“{link.text}”</span> : null}
          {link.alt ? <span className="links__alt">alt: {link.alt}</span> : null}
          {link.embedded ? (
            <span className="links__badge">carried in message</span>
          ) : null}
          <span className="links__part">part {link.part_path || 'root'}</span>
        </span>
      </div>

      <CopyButton value={link.url} label="Copy URL" />
    </li>
  )
}

/**
 * Group links by host, preserving first-appearance order.
 *
 * Hostless URLs (`mailto:`, `cid:`, relative paths) collect under one heading
 * placed last: they are real findings, but they are never the answer to "where
 * does this message point".
 */
function groupByHost(links: ExtractedLink[]): { host: string; links: ExtractedLink[] }[] {
  const byHost = new Map<string, ExtractedLink[]>()
  for (const link of links) {
    const host = link.host ?? NO_HOST
    const existing = byHost.get(host)
    if (existing) {
      existing.push(link)
    } else {
      byHost.set(host, [link])
    }
  }
  return [...byHost.entries()]
    .map(([host, hostLinks]) => ({ host, links: hostLinks }))
    .sort((a, b) => Number(a.host === NO_HOST) - Number(b.host === NO_HOST))
}
