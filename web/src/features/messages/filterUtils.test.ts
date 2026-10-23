import { describe, expect, it } from 'vitest'
import {
  buildFilterQuery,
  matchesMessage,
  parseFilterQuery,
} from './filterUtils'
import type { MessageSummary } from '../../api/client'

const testMsg1: MessageSummary = {
  id: 'msg-1',
  subject: 'Quarterly Financial Report',
  from: { display_name: 'Alice Johnson', address: 'alice@company.test' },
  to: [{ display_name: 'Bob Smith', address: 'bob@company.test' }],
  has_attachments: true,
  parse_ok: true,
  received_at: '2026-09-10T00:00:00Z',
  size: 2048,
  entries: [
    {
      entry_id: 'entry-1',
      mailbox_id: 'inbox',
      flags: [], // unread
      uid: 1,
    },
  ],
}

const testMsg2: MessageSummary = {
  id: 'msg-2',
  subject: 'Coffee Break & Chat',
  from: { display_name: 'Charlie Brown', address: 'charlie@peanuts.test' },
  to: [{ display_name: 'Alice Johnson', address: 'alice@company.test' }],
  has_attachments: false,
  parse_ok: true,
  received_at: '2026-09-10T01:00:00Z',
  size: 512,
  entries: [
    {
      entry_id: 'entry-2',
      mailbox_id: 'inbox',
      flags: ['\\Seen'], // read
      uid: 2,
    },
  ],
}

describe('filterUtils', () => {
  describe('parseFilterQuery', () => {
    it('parses empty query', () => {
      expect(parseFilterQuery('')).toEqual({ freeText: [] })
      expect(parseFilterQuery('   ')).toEqual({ freeText: [] })
    })

    it('parses from: and to: prefixes', () => {
      const parsed = parseFilterQuery('from:alice to:bob')
      expect(parsed.from).toBe('alice')
      expect(parsed.to).toBe('bob')
    })

    it('parses quoted values in prefixes', () => {
      const parsed = parseFilterQuery('from:"Alice Johnson" subject:"Quarterly Report"')
      expect(parsed.from).toBe('alice johnson')
      expect(parsed.subject).toBe('quarterly report')
    })

    it('parses is:unread and has:attachment', () => {
      const parsed = parseFilterQuery('is:unread has:attachment')
      expect(parsed.is).toBe('unread')
      expect(parsed.has).toBe('attachment')
    })

    it('parses free text alongside prefixes', () => {
      const parsed = parseFilterQuery('from:alice urgent "action required"')
      expect(parsed.from).toBe('alice')
      expect(parsed.freeText).toEqual(['urgent', 'action required'])
    })
  })

  describe('matchesMessage', () => {
    it('filters by from address and display name', () => {
      expect(matchesMessage(testMsg1, parseFilterQuery('from:alice'))).toBe(true)
      expect(matchesMessage(testMsg1, parseFilterQuery('from:company.test'))).toBe(true)
      expect(matchesMessage(testMsg1, parseFilterQuery('from:charlie'))).toBe(false)
    })

    it('filters by to recipient', () => {
      expect(matchesMessage(testMsg1, parseFilterQuery('to:bob'))).toBe(true)
      expect(matchesMessage(testMsg2, parseFilterQuery('to:alice'))).toBe(true)
      expect(matchesMessage(testMsg1, parseFilterQuery('to:charlie'))).toBe(false)
    })

    it('filters by subject', () => {
      expect(matchesMessage(testMsg1, parseFilterQuery('subject:financial'))).toBe(true)
      expect(matchesMessage(testMsg2, parseFilterQuery('subject:financial'))).toBe(false)
      expect(matchesMessage(testMsg2, parseFilterQuery('subject:"coffee break"'))).toBe(true)
    })

    it('filters by is:unread and is:read', () => {
      expect(matchesMessage(testMsg1, parseFilterQuery('is:unread'))).toBe(true)
      expect(matchesMessage(testMsg2, parseFilterQuery('is:unread'))).toBe(false)
      expect(matchesMessage(testMsg1, parseFilterQuery('is:read'))).toBe(false)
      expect(matchesMessage(testMsg2, parseFilterQuery('is:read'))).toBe(true)
    })

    it('filters by has:attachment', () => {
      expect(matchesMessage(testMsg1, parseFilterQuery('has:attachment'))).toBe(true)
      expect(matchesMessage(testMsg2, parseFilterQuery('has:attachment'))).toBe(false)
    })

    it('filters by free text matching anywhere', () => {
      expect(matchesMessage(testMsg1, parseFilterQuery('report'))).toBe(true)
      expect(matchesMessage(testMsg2, parseFilterQuery('peanuts'))).toBe(true)
      expect(matchesMessage(testMsg1, parseFilterQuery('nonexistent'))).toBe(false)
    })
  })

  describe('buildFilterQuery', () => {
    it('builds query from fields', () => {
      const q = buildFilterQuery({
        from: 'Alice Johnson',
        to: 'bob@test.com',
        subject: 'Report',
        unreadOnly: true,
        attachmentsOnly: true,
      })
      expect(q).toBe('from:"Alice Johnson" to:bob@test.com subject:Report is:unread has:attachment')
    })
  })
})
