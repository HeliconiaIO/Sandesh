import type { MessageSummary } from '../../api/client'

export interface ParsedFilter {
  from?: string
  to?: string
  subject?: string
  is?: string
  has?: string
  freeText: string[]
}

/**
 * Parses user filter strings supporting:
 * - from:<address or name>
 * - to:<address or name>
 * - subject:<text>
 * - is:unread | is:read | is:seen | is:unseen
 * - has:attachment | has:attachments
 * - quoted phrases: from:"Alice Smith"
 * - free text words/terms
 */
export function parseFilterQuery(input: string): ParsedFilter {
  const result: ParsedFilter = { freeText: [] }
  if (!input || !input.trim()) return result

  // Regex to match:
  // group 1 & 2/3: prefix:(quoted value | non-space value)
  // group 4: "quoted free text"
  // group 5: unquoted word
  const regex = /(\b\w+):(?:"([^"]*)"|(\S+))|"([^"]*)"|(\S+)/g
  let match: RegExpExecArray | null

  while ((match = regex.exec(input)) !== null) {
    if (match[1]) {
      const key = match[1].toLowerCase()
      const val = (match[2] ?? match[3] ?? '').toLowerCase().trim()
      if (key === 'from') result.from = val
      else if (key === 'to') result.to = val
      else if (key === 'subject') result.subject = val
      else if (key === 'is') result.is = val
      else if (key === 'has') result.has = val
      else {
        result.freeText.push(`${key}:${val}`)
      }
    } else if (match[4] !== undefined) {
      const phrase = match[4].toLowerCase().trim()
      if (phrase) result.freeText.push(phrase)
    } else if (match[5] !== undefined) {
      const word = match[5].toLowerCase().trim()
      if (word) result.freeText.push(word)
    }
  }

  return result
}

/**
 * Matches a MessageSummary against a ParsedFilter.
 */
export function matchesMessage(
  msg: MessageSummary,
  parsed: ParsedFilter,
  mailboxId?: string,
): boolean {
  // 1. from:
  if (parsed.from) {
    const fromAddr = msg.from?.address?.toLowerCase() ?? ''
    const fromName = msg.from?.display_name?.toLowerCase() ?? ''
    if (!fromAddr.includes(parsed.from) && !fromName.includes(parsed.from)) {
      return false
    }
  }

  // 2. to:
  if (parsed.to) {
    const toMatch = msg.to?.some((t) => {
      const toAddr = t.address?.toLowerCase() ?? ''
      const toName = t.display_name?.toLowerCase() ?? ''
      return toAddr.includes(parsed.to!) || toName.includes(parsed.to!)
    })
    if (!toMatch) return false
  }

  // 3. subject:
  if (parsed.subject) {
    const subject = msg.subject?.toLowerCase() ?? ''
    if (!subject.includes(parsed.subject)) {
      return false
    }
  }

  // 4. is:
  if (parsed.is) {
    const relevant =
      (mailboxId ? msg.entries.find((e) => e.mailbox_id === mailboxId) : null) ??
      msg.entries[0]
    const isSeen = Boolean(relevant?.flags.includes('\\Seen'))
    if (parsed.is === 'unread' || parsed.is === 'unseen') {
      if (isSeen) return false
    } else if (parsed.is === 'read' || parsed.is === 'seen') {
      if (!isSeen) return false
    }
  }

  // 5. has:
  if (parsed.has) {
    if (parsed.has === 'attachment' || parsed.has === 'attachments') {
      if (!msg.has_attachments) return false
    }
  }

  // 6. Free text terms (all must match somewhere in subject, from, or to)
  for (const term of parsed.freeText) {
    const inSubject = msg.subject?.toLowerCase().includes(term)
    const inFrom =
      msg.from?.address?.toLowerCase().includes(term) ||
      msg.from?.display_name?.toLowerCase().includes(term)
    const inTo = msg.to?.some(
      (t) =>
        t.address?.toLowerCase().includes(term) ||
        t.display_name?.toLowerCase().includes(term),
    )
    if (!inSubject && !inFrom && !inTo) {
      return false
    }
  }

  return true
}

/**
 * Serializes filter fields into a query string.
 */
export function buildFilterQuery(fields: {
  from?: string
  to?: string
  subject?: string
  unreadOnly?: boolean
  attachmentsOnly?: boolean
  freeText?: string
}): string {
  const parts: string[] = []
  if (fields.from?.trim()) {
    const val = fields.from.trim()
    parts.push(val.includes(' ') ? `from:"${val}"` : `from:${val}`)
  }
  if (fields.to?.trim()) {
    const val = fields.to.trim()
    parts.push(val.includes(' ') ? `to:"${val}"` : `to:${val}`)
  }
  if (fields.subject?.trim()) {
    const val = fields.subject.trim()
    parts.push(val.includes(' ') ? `subject:"${val}"` : `subject:${val}`)
  }
  if (fields.unreadOnly) {
    parts.push('is:unread')
  }
  if (fields.attachmentsOnly) {
    parts.push('has:attachment')
  }
  if (fields.freeText?.trim()) {
    parts.push(fields.freeText.trim())
  }
  return parts.join(' ')
}
