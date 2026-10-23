import { useEffect, useRef, useState, type ReactNode } from 'react'
import { AlertCircle, AlertTriangle, CheckCircle2, Info, X } from 'lucide-react'
import './DialogModal.css'

export type DialogVariant = 'danger' | 'warning' | 'info' | 'success' | 'default'

export interface DialogModalProps {
  isOpen: boolean
  type: 'confirm' | 'alert' | 'prompt'
  title?: string | undefined
  message: ReactNode
  confirmText?: string | undefined
  cancelText?: string | undefined
  variant?: DialogVariant | undefined
  defaultValue?: string | undefined
  placeholder?: string | undefined
  onConfirm: (inputValue?: string) => void
  onCancel: () => void
}

export function DialogModal({
  isOpen,
  type,
  title,
  message,
  confirmText = type === 'alert' ? 'OK' : 'Confirm',
  cancelText = 'Cancel',
  variant = type === 'confirm' ? 'default' : 'info',
  defaultValue = '',
  placeholder = '',
  onConfirm,
  onCancel,
}: DialogModalProps) {
  const [promptValue, setPromptValue] = useState(defaultValue)
  const cancelBtnRef = useRef<HTMLButtonElement>(null)
  const confirmBtnRef = useRef<HTMLButtonElement>(null)
  const inputRef = useRef<HTMLInputElement>(null)
  const dialogRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    if (isOpen) {
      setPromptValue(defaultValue)
    }
  }, [isOpen, defaultValue])

  // Manage initial focus
  useEffect(() => {
    if (!isOpen) return

    const timer = setTimeout(() => {
      if (type === 'prompt' && inputRef.current) {
        inputRef.current.focus()
        inputRef.current.select()
      } else if (variant === 'danger' && cancelBtnRef.current) {
        // Safe UX: focus Cancel by default on destructive actions
        cancelBtnRef.current.focus()
      } else if (confirmBtnRef.current) {
        confirmBtnRef.current.focus()
      }
    }, 20)

    return () => clearTimeout(timer)
  }, [isOpen, type, variant])

  // Keyboard navigation & Focus Trap
  useEffect(() => {
    if (!isOpen) return

    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.preventDefault()
        e.stopPropagation()
        onCancel()
        return
      }

      if (e.key === 'Enter' && type !== 'prompt') {
        e.preventDefault()
        e.stopPropagation()
        onConfirm()
        return
      }

      if (e.key === 'Tab' && dialogRef.current) {
        const focusable = dialogRef.current.querySelectorAll<HTMLElement>(
          'button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"])',
        )
        if (focusable.length === 0) return

        const first = focusable[0]
        const last = focusable[focusable.length - 1]

        if (first && last) {
          if (e.shiftKey && document.activeElement === first) {
            e.preventDefault()
            last.focus()
          } else if (!e.shiftKey && document.activeElement === last) {
            e.preventDefault()
            first.focus()
          }
        }
      }
    }

    window.addEventListener('keydown', handleKeyDown, { capture: true })
    return () => window.removeEventListener('keydown', handleKeyDown, { capture: true })
  }, [isOpen, type, onCancel, onConfirm])

  if (!isOpen) return null

  const handleOverlayClick = (e: React.MouseEvent<HTMLDivElement>) => {
    if (e.target === e.currentTarget) {
      onCancel()
    }
  }

  const handlePromptSubmit = (e: React.FormEvent) => {
    e.preventDefault()
    onConfirm(promptValue)
  }

  const renderIcon = () => {
    switch (variant) {
      case 'danger':
        return <AlertTriangle size={18} aria-hidden="true" />
      case 'warning':
        return <AlertCircle size={18} aria-hidden="true" />
      case 'success':
        return <CheckCircle2 size={18} aria-hidden="true" />
      case 'info':
      default:
        return <Info size={18} aria-hidden="true" />
    }
  }

  const defaultTitle =
    title ||
    (type === 'alert'
      ? 'Notice'
      : type === 'prompt'
        ? 'Input Required'
        : 'Are you sure?')

  return (
    <div
      className="dialog-overlay"
      onClick={handleOverlayClick}
      role="presentation"
    >
      <div
        ref={dialogRef}
        className="dialog-card"
        role="alertdialog"
        aria-modal="true"
        aria-labelledby="dialog-title"
        aria-describedby="dialog-desc"
      >
        <div className="dialog-header">
          <div className={`dialog-icon-wrap dialog-icon-wrap--${variant}`}>
            {renderIcon()}
          </div>
          <div className="dialog-title-wrap">
            <h2 id="dialog-title" className="dialog-title">
              {defaultTitle}
            </h2>
          </div>
          <button
            type="button"
            className="dialog-close-btn"
            onClick={onCancel}
            aria-label="Close dialog"
          >
            <X size={15} aria-hidden="true" />
          </button>
        </div>

        <div className="dialog-body" id="dialog-desc">
          <div className="dialog-message">{message}</div>

          {type === 'prompt' && (
            <form onSubmit={handlePromptSubmit} className="dialog-prompt-wrap">
              <input
                ref={inputRef}
                type="text"
                className="dialog-prompt-input"
                value={promptValue}
                placeholder={placeholder}
                onChange={(e) => setPromptValue(e.target.value)}
              />
            </form>
          )}
        </div>

        <div className="dialog-footer">
          {type !== 'alert' && (
            <button
              ref={cancelBtnRef}
              type="button"
              className="dialog-btn dialog-btn--cancel"
              onClick={onCancel}
            >
              {cancelText}
            </button>
          )}
          <button
            ref={confirmBtnRef}
            type="button"
            className={`dialog-btn ${variant === 'danger' ? 'dialog-btn--danger' : 'dialog-btn--primary'}`}
            onClick={() => onConfirm(type === 'prompt' ? promptValue : undefined)}
          >
            {confirmText}
          </button>
        </div>
      </div>
    </div>
  )
}
