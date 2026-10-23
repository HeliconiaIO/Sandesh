import { useCallback, useEffect, useRef, useState } from 'react'
import { CornerUpLeft, Loader2, Maximize2, Minus, Send, X } from 'lucide-react'

import { useAccounts, useSendMessage } from '../../hooks/queries'
import { useKeyboardShortcutsContext } from '../../context/KeyboardShortcutsContext'
import { useDialog } from '../../context/DialogContext'
import { RichTextEditor } from './RichTextEditor'
import './MessageComposer.css'

export const DRAFT_STORAGE_KEY = 'sandesh_composer_draft'

export interface ComposerInitialValues {
  from?: string | undefined
  to?: string | undefined
  cc?: string | undefined
  bcc?: string | undefined
  subject?: string | undefined
  body?: string | undefined
  isHtml?: boolean | undefined
  inReplyTo?: string | undefined
  references?: string | undefined
  replyContext?: {
    from: string
    subject: string
  } | undefined
}

export interface MessageComposerProps {
  isOpen: boolean
  initialValues?: ComposerInitialValues | undefined
  onClose: () => void
}

interface StoredDraft {
  from?: string
  to?: string
  cc?: string
  bcc?: string
  subject?: string
  body?: string
  isHtml?: boolean
  updatedAt?: number
}

export function MessageComposer({ isOpen, initialValues, onClose }: MessageComposerProps) {
  const accountsQuery = useAccounts()
  const sendMessage = useSendMessage()
  const { formatKey } = useKeyboardShortcutsContext()
  const { confirm } = useDialog()

  const defaultFrom =
    initialValues?.from ||
    accountsQuery.data?.items[0]?.addresses[0] ||
    (accountsQuery.data?.items[0]?.username
      ? `${accountsQuery.data.items[0].username}@sandesh.test`
      : 'dev@sandesh.test')

  const [from, setFrom] = useState(defaultFrom)
  const [to, setTo] = useState('')
  const [cc, setCc] = useState('')
  const [bcc, setBcc] = useState('')
  const [showCcBcc, setShowCcBcc] = useState(false)
  const [subject, setSubject] = useState('')
  const [body, setBody] = useState('')
  const [isHtml, setIsHtml] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [isMinimized, setIsMinimized] = useState(false)
  const [hasRestoredDraft, setHasRestoredDraft] = useState(false)

  const toInputRef = useRef<HTMLInputElement>(null)

  useEffect(() => {
    if (isOpen) {
      setIsMinimized(false)
      setError(null)

      if (initialValues) {
        setFrom(defaultFrom)
        setTo(initialValues.to || '')
        setCc(initialValues.cc || '')
        setBcc(initialValues.bcc || '')
        setShowCcBcc(Boolean(initialValues.cc || initialValues.bcc))
        setSubject(initialValues.subject || '')
        setBody(initialValues.body || '')
        setIsHtml(initialValues.isHtml || false)
        setHasRestoredDraft(false)
      } else {
        // Attempt to restore saved draft from localStorage
        let restored = false
        try {
          const rawDraft = localStorage.getItem(DRAFT_STORAGE_KEY)
          if (rawDraft) {
            const parsed = JSON.parse(rawDraft) as StoredDraft
            if (parsed.to || parsed.subject || parsed.body) {
              setFrom(parsed.from || defaultFrom)
              setTo(parsed.to || '')
              setCc(parsed.cc || '')
              setBcc(parsed.bcc || '')
              setShowCcBcc(Boolean(parsed.cc || parsed.bcc))
              setSubject(parsed.subject || '')
              setBody(parsed.body || '')
              setIsHtml(Boolean(parsed.isHtml))
              setHasRestoredDraft(true)
              restored = true
            }
          }
        } catch {
          // ignore parsing error
        }

        if (!restored) {
          setFrom(defaultFrom)
          setTo('')
          setCc('')
          setBcc('')
          setShowCcBcc(false)
          setSubject('')
          setBody('')
          setIsHtml(false)
          setHasRestoredDraft(false)
        }
      }

      // Focus recipient input on open if not minimized
      setTimeout(() => {
        toInputRef.current?.focus()
      }, 50)
    }
  }, [isOpen, initialValues, defaultFrom])

  // Auto-save draft changes to localStorage for new messages
  useEffect(() => {
    if (!isOpen || initialValues?.replyContext) return

    if (to.trim() || subject.trim() || body.trim()) {
      try {
        const draft: StoredDraft = {
          from,
          to,
          cc,
          bcc,
          subject,
          body,
          isHtml,
          updatedAt: Date.now(),
        }
        localStorage.setItem(DRAFT_STORAGE_KEY, JSON.stringify(draft))
      } catch {
        // ignore localStorage quota errors
      }
    }
  }, [isOpen, initialValues, from, to, cc, bcc, subject, body, isHtml])

  const handleDiscardDraft = () => {
    try {
      localStorage.removeItem(DRAFT_STORAGE_KEY)
    } catch {
      // ignore
    }
    setTo('')
    setCc('')
    setBcc('')
    setShowCcBcc(false)
    setSubject('')
    setBody('')
    setIsHtml(false)
    setHasRestoredDraft(false)
  }

  const handleSafeClose = useCallback(async () => {
    const hasUnsaved = Boolean(
      (to && to !== (initialValues?.to || '')) ||
      (subject && subject !== (initialValues?.subject || '')) ||
      (body && body !== (initialValues?.body || ''))
    )

    if (hasUnsaved) {
      const ok = await confirm({
        title: 'Discard draft?',
        message: 'You have unsaved changes. Discard this message?',
        confirmText: 'Discard',
        cancelText: 'Keep Editing',
        variant: 'danger',
      })
      if (ok) {
        try {
          localStorage.removeItem(DRAFT_STORAGE_KEY)
        } catch {
          // ignore
        }
        setIsMinimized(false)
        onClose()
      }
    } else {
      try {
        localStorage.removeItem(DRAFT_STORAGE_KEY)
      } catch {
        // ignore
      }
      setIsMinimized(false)
      onClose()
    }
  }, [to, subject, body, initialValues, onClose, confirm])

  const handleSendRef = useRef<(() => void) | null>(null)

  // Intercept Escape key and Mod+Enter in capture phase so all editors and inputs are covered
  useEffect(() => {
    if (!isOpen) return

    const handleGlobalKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        // If a higher dialog (like confirmation modal) is open, let it handle Escape
        if (document.querySelector('.dialog-overlay')) {
          return
        }
        e.preventDefault()
        e.stopPropagation()
        e.stopImmediatePropagation()
        handleSafeClose()
      } else if ((e.ctrlKey || e.metaKey) && e.key === 'Enter') {
        if (document.querySelector('.dialog-overlay')) {
          return
        }
        e.preventDefault()
        e.stopPropagation()
        e.stopImmediatePropagation()
        handleSendRef.current?.()
      }
    }

    window.addEventListener('keydown', handleGlobalKeyDown, { capture: true })
    return () => {
      window.removeEventListener('keydown', handleGlobalKeyDown, { capture: true })
    }
  }, [isOpen, handleSafeClose])

  const handleSend = async (e?: React.FormEvent) => {
    if (e) e.preventDefault()

    if (!from.trim()) {
      setError("Please specify a 'From' sender address.")
      return
    }

    const recipients = to
      .split(/[,;]/)
      .map((s) => s.trim())
      .filter(Boolean)

    if (recipients.length === 0) {
      setError("Please specify at least one recipient in 'To'.")
      return
    }

    const ccRecipients = cc
      .split(/[,;]/)
      .map((s) => s.trim())
      .filter(Boolean)

    const bccRecipients = bcc
      .split(/[,;]/)
      .map((s) => s.trim())
      .filter(Boolean)

    setError(null)

    try {
      await sendMessage.mutateAsync({
        from: from.trim(),
        to: recipients,
        cc: ccRecipients,
        bcc: bccRecipients,
        subject: subject.trim(),
        text_body: isHtml ? null : (body || null),
        html_body: isHtml ? (body || null) : null,
        in_reply_to: initialValues?.inReplyTo ?? null,
        references: initialValues?.references ?? null,
      })

      try {
        localStorage.removeItem(DRAFT_STORAGE_KEY)
      } catch {
        // ignore
      }
      setIsMinimized(false)
      onClose()
    } catch (err: unknown) {
      if (err instanceof Error) {
        setError(err.message)
      } else {
        setError('Failed to send message. Please check the inputs.')
      }
    }
  }

  handleSendRef.current = () => {
    if (!sendMessage.isPending) {
      void handleSend()
    }
  }

  const handleSaveDraft = async () => {
    if (!from.trim()) {
      setError("Please specify a 'From' sender address.")
      return
    }

    const recipients = to
      .split(/[,;]/)
      .map((s) => s.trim())
      .filter(Boolean)

    const ccRecipients = cc
      .split(/[,;]/)
      .map((s) => s.trim())
      .filter(Boolean)

    const bccRecipients = bcc
      .split(/[,;]/)
      .map((s) => s.trim())
      .filter(Boolean)

    setError(null)

    try {
      await sendMessage.mutateAsync({
        from: from.trim(),
        to: recipients,
        cc: ccRecipients,
        bcc: bccRecipients,
        subject: subject.trim() || '(Draft)',
        text_body: isHtml ? null : (body || null),
        html_body: isHtml ? (body || null) : null,
        in_reply_to: initialValues?.inReplyTo ?? null,
        references: initialValues?.references ?? null,
        is_draft: true,
      })

      try {
        localStorage.removeItem(DRAFT_STORAGE_KEY)
      } catch {
        // ignore
      }
      setIsMinimized(false)
      onClose()
    } catch (err: unknown) {
      if (err instanceof Error) {
        setError(err.message)
      } else {
        setError('Failed to save draft.')
      }
    }
  }

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if ((e.ctrlKey || e.metaKey) && e.key === 'Enter') {
      e.preventDefault()
      void handleSend()
    }
  }

  const isReplying = Boolean(initialValues?.replyContext)

  if (!isOpen) return null

  return (
    <div
      className={`composer-overlay ${isMinimized ? 'composer-overlay--minimized' : ''}`}
      role="dialog"
      aria-modal={!isMinimized}
    >
      <div
        className={`composer-dialog ${isMinimized ? 'composer-dialog--minimized' : ''}`}
        onClick={(e) => {
          e.stopPropagation()
          if (isMinimized) {
            setIsMinimized(false)
          }
        }}
        onKeyDown={handleKeyDown}
      >
        <header className="composer-header">
          <h3 className="composer-header__title">
            {isReplying ? (
              <>
                <CornerUpLeft size={16} aria-hidden="true" />
                <span className="composer-header__subject">
                  {subject.trim() || 'Reply'}
                </span>
              </>
            ) : (
              <>
                <Send size={16} aria-hidden="true" />
                <span className="composer-header__subject">
                  {subject.trim() || 'New Message'}
                </span>
              </>
            )}
          </h3>
          <div className="composer-header__controls">
            <button
              type="button"
              className="composer-ctrl-btn"
              onClick={(e) => {
                e.stopPropagation()
                setIsMinimized(!isMinimized)
              }}
              aria-label={isMinimized ? 'Expand composer' : 'Minimize composer'}
              title={isMinimized ? 'Expand' : 'Minimize'}
            >
              {isMinimized ? (
                <Maximize2 size={13} aria-hidden="true" />
              ) : (
                <Minus size={13} aria-hidden="true" />
              )}
            </button>
            <button
              type="button"
              className="composer-ctrl-btn composer-ctrl-btn--close"
              onClick={(e) => {
                e.stopPropagation()
                handleSafeClose()
              }}
              aria-label="Close composer"
              title="Close (Esc)"
            >
              <X size={15} aria-hidden="true" />
            </button>
          </div>
        </header>

        {!isMinimized ? (
          <>
            {isReplying && initialValues?.replyContext ? (
              <div className="composer-reply-banner">
                <CornerUpLeft size={13} aria-hidden="true" />
                <span>
                  Replying to <strong>{initialValues.replyContext.from}</strong> &bull;{' '}
                  {initialValues.replyContext.subject}
                </span>
              </div>
            ) : null}

            {hasRestoredDraft ? (
              <div className="composer-draft-banner">
                <span>Restored saved draft</span>
                <button
                  type="button"
                  className="composer-draft-discard-btn"
                  onClick={handleDiscardDraft}
                >
                  Discard Draft
                </button>
              </div>
            ) : null}

            <form className="composer-form" onSubmit={(e) => void handleSend(e)}>
          {error ? (
            <div className="composer-error" role="alert">
              {error}
            </div>
          ) : null}

          <div className="composer-fields">
            <div className="composer-field-row">
              <label className="composer-field-label" htmlFor="composer-from">
                From
              </label>
              <input
                id="composer-from"
                className="composer-field-input"
                type="text"
                value={from}
                onChange={(e) => setFrom(e.target.value)}
                placeholder="sender@sandesh.test"
                required
              />
            </div>

            <div className="composer-field-row">
              <label className="composer-field-label" htmlFor="composer-to">
                To
              </label>
              <input
                ref={toInputRef}
                id="composer-to"
                className="composer-field-input"
                type="text"
                value={to}
                onChange={(e) => setTo(e.target.value)}
                placeholder="recipient@example.com (comma separated)"
                required
              />
              {!showCcBcc ? (
                <button
                  type="button"
                  className="composer-toggle-btn"
                  onClick={() => setShowCcBcc(true)}
                >
                  Cc / Bcc
                </button>
              ) : null}
            </div>

            {showCcBcc ? (
              <>
                <div className="composer-field-row">
                  <label className="composer-field-label" htmlFor="composer-cc">
                    Cc
                  </label>
                  <input
                    id="composer-cc"
                    className="composer-field-input"
                    type="text"
                    value={cc}
                    onChange={(e) => setCc(e.target.value)}
                    placeholder="cc@example.com"
                  />
                </div>

                <div className="composer-field-row">
                  <label className="composer-field-label" htmlFor="composer-bcc">
                    Bcc
                  </label>
                  <input
                    id="composer-bcc"
                    className="composer-field-input"
                    type="text"
                    value={bcc}
                    onChange={(e) => setBcc(e.target.value)}
                    placeholder="bcc@example.com"
                  />
                </div>
              </>
            ) : null}

            <div className="composer-field-row">
              <label className="composer-field-label" htmlFor="composer-subject">
                Subject
              </label>
              <input
                id="composer-subject"
                className="composer-field-input"
                type="text"
                value={subject}
                onChange={(e) => setSubject(e.target.value)}
                placeholder="Subject"
              />
            </div>
          </div>

          <div className="composer-toolbar">
            <div className="composer-segmented">
              <button
                type="button"
                className={`composer-segmented-btn ${!isHtml ? 'composer-segmented-btn--active' : ''}`}
                onClick={() => setIsHtml(false)}
              >
                Plain Text
              </button>
              <button
                type="button"
                className={`composer-segmented-btn ${isHtml ? 'composer-segmented-btn--active' : ''}`}
                onClick={() => setIsHtml(true)}
              >
                HTML
              </button>
            </div>
            {isHtml ? (
              <span className="composer-toolbar-hint">Rich Text / HTML</span>
            ) : null}
          </div>

          <div className="composer-body-container">
            {isHtml ? (
              <RichTextEditor
                value={body}
                onChange={setBody}
                placeholder="Write HTML message body here..."
              />
            ) : (
              <textarea
                id="composer-body"
                className="composer-body-input"
                value={body}
                onChange={(e) => setBody(e.target.value)}
                placeholder="Write message body here..."
                rows={10}
              />
            )}
          </div>

          <footer className="composer-footer">
            <span className="composer-shortcut-hint">
              <kbd>{formatKey('Mod')}</kbd> + <kbd>{formatKey('Enter')}</kbd> to send
            </span>
            <div className="composer-actions">
              <button
                type="button"
                className="button"
                onClick={handleSafeClose}
                disabled={sendMessage.isPending}
              >
                Cancel
              </button>
              <button
                type="button"
                className="button"
                onClick={() => void handleSaveDraft()}
                disabled={sendMessage.isPending}
              >
                Save as Draft
              </button>
              <button
                type="submit"
                className="button button--primary"
                disabled={sendMessage.isPending}
              >
                {sendMessage.isPending ? (
                  <>
                    <Loader2 size={13} className="spin" aria-hidden="true" />
                    <span>Sending...</span>
                  </>
                ) : (
                  <>
                    <Send size={13} aria-hidden="true" />
                    <span>Send</span>
                  </>
                )}
              </button>
            </div>
          </footer>
        </form>
          </>
        ) : null}
      </div>
    </div>
  )
}
