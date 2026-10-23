/**
 * Confirmation dialog for deleting an account.
 */

import { AlertTriangle, Trash2, X } from 'lucide-react'
import type { Account } from '../../api/client'
import { useDeleteAccount } from '../../hooks/queries'
import { useState } from 'react'
import './AccountModal.css'

interface DeleteAccountDialogProps {
  account: Account
  onClose: () => void
}

export function DeleteAccountDialog({ account, onClose }: DeleteAccountDialogProps) {
  const deleteAccount = useDeleteAccount()
  const [errorMsg, setErrorMsg] = useState<string | null>(null)

  const handleDelete = () => {
    setErrorMsg(null)
    deleteAccount.mutate(account.id, {
      onSuccess: onClose,
      onError: (err: unknown) => {
        const detail =
          (err as { detail?: string })?.detail ||
          (err as Error)?.message ||
          'Failed to delete account'
        setErrorMsg(detail)
      },
    })
  }

  return (
    <div className="account-modal__backdrop" onClick={onClose} role="presentation">
      <div
        className="account-modal account-modal--danger"
        role="alertdialog"
        aria-modal="true"
        aria-labelledby="delete-account-title"
        onClick={(e) => e.stopPropagation()}
      >
        <header className="account-modal__header">
          <div className="account-modal__title-wrap">
            <AlertTriangle size={16} className="account-modal__icon--danger" aria-hidden="true" />
            <h2 id="delete-account-title" className="account-modal__title">
              Delete Account &ldquo;{account.username}&rdquo;?
            </h2>
          </div>
          <button
            type="button"
            className="account-modal__close-btn"
            onClick={onClose}
            aria-label="Close dialog"
          >
            <X size={14} aria-hidden="true" />
          </button>
        </header>

        <div className="account-modal__body">
          {errorMsg && (
            <div className="account-modal__error" role="alert">
              {errorMsg}
            </div>
          )}

          <p className="account-modal__warning-text">
            Are you sure you want to delete the account <strong>{account.username}</strong>?
          </p>
          <ul className="account-modal__warning-list">
            <li>All mailboxes (INBOX, Sent, custom folders) under this account will be permanently deleted.</li>
            <li>All message placements in this account will be removed.</li>
            <li>Bound email addresses ({account.addresses.length > 0 ? account.addresses.join(', ') : 'none'}) will be unassigned.</li>
            {account.is_catch_all && (
              <li className="account-modal__warning-item--highlight">
                <strong>Warning:</strong> This is currently marked as the <strong>Catch-All</strong> account. Deleting it will leave Sandesh with no fallback catch-all destination unless another account is configured.
              </li>
            )}
          </ul>
        </div>

        <footer className="account-modal__footer">
          <button
            type="button"
            className="button button--ghost"
            onClick={onClose}
            disabled={deleteAccount.isPending}
          >
            Cancel
          </button>
          <button
            type="button"
            className="button button--danger"
            onClick={handleDelete}
            disabled={deleteAccount.isPending}
          >
            {deleteAccount.isPending ? (
              'Deleting...'
            ) : (
              <>
                <Trash2 size={14} aria-hidden="true" />
                <span>Delete Account</span>
              </>
            )}
          </button>
        </footer>
      </div>
    </div>
  )
}
