/**
 * Test fixtures shaped like real API responses.
 *
 * Typed as the generated contract types, so a backend change that alters a DTO
 * breaks these at compile time rather than leaving tests that assert against a
 * shape the server no longer sends.
 */

import type { MessageDetail, MessageSummary, MimeTree } from '../api/client'

export function summary(overrides: Partial<MessageSummary> = {}): MessageSummary {
  return {
    id: '01920000-0000-7000-8000-000000000001',
    subject: 'Password reset',
    from: { address: 'noreply@app.test', display_name: 'App' },
    to: [{ address: 'dev@example.test', display_name: null }],
    received_at: new Date().toISOString(),
    size: 4821,
    has_attachments: false,
    parse_ok: true,
    entries: [
      {
        entry_id: '01920000-0000-7000-8000-0000000000e1',
        mailbox_id: '01920000-0000-7000-8000-0000000000b1',
        mailbox_name: 'INBOX',
        uid: 1,
        flags: [],
      },
    ],
    ...overrides,
  }
}

export function detail(overrides: Partial<MessageDetail> = {}): MessageDetail {
  return {
    ...summary(),
    headers: [
      { name: 'Subject', value: 'Password reset', raw_value: 'Password reset' },
      { name: 'From', value: 'noreply@app.test', raw_value: 'noreply@app.test' },
    ],
    attachments: [],
    diagnostics: [],
    source: 'smtp',
    ...overrides,
  }
}

export function mimeTree(overrides: Partial<MimeTree> = {}): MimeTree {
  return {
    parts: [
      {
        path: '',
        url_path: 'root',
        content_type: 'multipart/alternative',
        charset: null,
        encoding: '7bit',
        filename: null,
        body_offset: 0,
        body_length: 512,
        decoded_size: null,
        is_attachment: false,
        children: ['1', '2'],
      },
      {
        path: '1',
        url_path: '1',
        content_type: 'text/plain',
        charset: 'utf-8',
        encoding: '7bit',
        filename: null,
        body_offset: 120,
        body_length: 64,
        decoded_size: 64,
        is_attachment: false,
        children: [],
      },
      {
        path: '2',
        url_path: '2',
        content_type: 'text/html',
        charset: 'utf-8',
        encoding: 'quoted-printable',
        filename: null,
        body_offset: 200,
        body_length: 180,
        decoded_size: 176,
        is_attachment: false,
        children: [],
      },
    ],
    text_parts: ['1'],
    html_parts: ['2'],
    parse_ok: true,
    diagnostics: [],
    ...overrides,
  }
}
