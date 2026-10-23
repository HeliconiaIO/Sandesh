/**
 * Modal dialog for creating or editing accounts.
 */

import { useState } from 'react'
import { Check, Eye, EyeOff, Sparkles, User, X } from 'lucide-react'
import type { Account } from '../../api/client'
import { useCreateAccount, usePatchAccount } from '../../hooks/queries'
import { CopyButton } from '../../components/CopyButton'
import './AccountModal.css'

interface AccountModalProps {
  account?: Account | undefined
  onClose: () => void
}

export function AccountModal({ account, onClose }: AccountModalProps) {
  const isEditing = Boolean(account)
  const createAccount = useCreateAccount()
  const patchAccount = usePatchAccount()

  const [username, setUsername] = useState(account?.username ?? '')
  const [password, setPassword] = useState('')
  const [showPassword, setShowPassword] = useState(false)
  const [addressesInput, setAddressesInput] = useState(
    account?.addresses.join(', ') ?? '',
  )
  const [isCatchAll, setIsCatchAll] = useState(account?.is_catch_all ?? false)
  const [errorMsg, setErrorMsg] = useState<string | null>(null)

  const isPending = createAccount.isPending || patchAccount.isPending

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault()
    setErrorMsg(null)

    const trimmedUser = username.trim()
    if (!isEditing && !trimmedUser) {
      setErrorMsg('Username is required')
      return
    }

    const addresses = addressesInput
      .split(',')
      .map((a) => a.trim())
      .filter(Boolean)

    if (isEditing && account) {
      // Find new addresses to add
      const currentAddrs = new Set(account.addresses)
      const toAdd = addresses.filter((a) => !currentAddrs.has(a))
      const toRemove = account.addresses.filter((a) => !addresses.includes(a))

      patchAccount.mutate(
        {
          id: account.id,
          ...(password.trim() ? { password: password.trim() } : {}),
          ...(toAdd.length > 0 ? { add_addresses: toAdd } : {}),
          ...(toRemove.length > 0 ? { remove_addresses: toRemove } : {}),
          is_catch_all: isCatchAll,
        },
        {
          onSuccess: onClose,
          onError: (err: unknown) => {
            const detail =
              (err as { detail?: string })?.detail ||
              (err as Error)?.message ||
              'Failed to update account'
            setErrorMsg(detail)
          },
        },
      )
    } else {
      createAccount.mutate(
        {
          username: trimmedUser,
          ...(password.trim() ? { password: password.trim() } : {}),
          ...(addresses.length > 0 ? { addresses } : {}),
          is_catch_all: isCatchAll,
        },
        {
          onSuccess: onClose,
          onError: (err: unknown) => {
            const detail =
              (err as { detail?: string })?.detail ||
              (err as Error)?.message ||
              'Failed to create account'
            setErrorMsg(detail)
          },
        },
      )
    }
  }

  return (
    <div className="account-modal__backdrop" onClick={onClose} role="presentation">
      <div
        className="account-modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="account-modal-title"
        onClick={(e) => e.stopPropagation()}
      >
        <header className="account-modal__header">
          <div className="account-modal__title-wrap">
            <User size={16} className="account-modal__icon" aria-hidden="true" />
            <h2 id="account-modal-title" className="account-modal__title">
              {isEditing ? `Edit Account: ${account?.username}` : 'Create New Account'}
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

        <form onSubmit={handleSubmit} className="account-modal__form">
          {errorMsg && (
            <div className="account-modal__error" role="alert">
              {errorMsg}
            </div>
          )}

          <div className="account-modal__field">
            <label htmlFor="acc-username" className="account-modal__label">
              Username {!isEditing && <span className="account-modal__required">*</span>}
            </label>
            <input
              id="acc-username"
              name="username"
              type="text"
              className="account-modal__input"
              placeholder="e.g. bob, support, billing"
              value={username}
              onChange={(e) => setUsername(e.target.value)}
              disabled={isEditing || isPending}
              autoFocus={!isEditing}
              required={!isEditing}
              autoComplete="username"
            />
            {isEditing && (
              <span className="account-modal__hint">
                Account username cannot be changed after creation.
              </span>
            )}
          </div>

          <div className="account-modal__field">
            <label htmlFor="acc-addresses" className="account-modal__label">
              Bound Email Address(es)
            </label>
            <input
              id="acc-addresses"
              name="addresses"
              type="text"
              className="account-modal__input"
              placeholder="e.g. bob@example.com, sales@test.local (comma-separated)"
              value={addressesInput}
              onChange={(e) => setAddressesInput(e.target.value)}
              disabled={isPending}
            />
            <span className="account-modal__hint">
              Incoming emails matching these addresses route directly to this account. Separate multiple with commas.
            </span>
          </div>

          <div className="account-modal__field">
            <div className="account-modal__password-label-row">
              <label htmlFor="acc-password" className="account-modal__label">
                {isEditing ? 'New Password (leave blank to keep current)' : 'Password (Optional)'}
              </label>
              {account?.has_password ? (
                <span className="account-modal__pw-status account-modal__pw-status--set">
                  ✓ Password Configured
                </span>
              ) : account ? (
                <span className="account-modal__pw-status account-modal__pw-status--none">
                  No Password Set
                </span>
              ) : null}
            </div>
            <div className="account-modal__password-wrapper">
              <input
                id="acc-password"
                name="password"
                type={showPassword ? 'text' : 'password'}
                className="account-modal__input account-modal__input--password"
                placeholder={isEditing ? 'Enter new password...' : 'Password for SMTP / IMAP / POP3'}
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                disabled={isPending}
                autoComplete="new-password"
              />
              <div className="account-modal__password-actions">
                <button
                  type="button"
                  className="account-modal__pw-btn"
                  onClick={() => setShowPassword((prev) => !prev)}
                  title={showPassword ? 'Hide password' : 'Show password'}
                  aria-label={showPassword ? 'Hide password' : 'Show password'}
                  tabIndex={-1}
                >
                  {showPassword ? <EyeOff size={14} aria-hidden="true" /> : <Eye size={14} aria-hidden="true" />}
                </button>
                {password ? (
                  <CopyButton value={password} label="password" className="account-modal__pw-btn" />
                ) : null}
                <button
                  type="button"
                  className="account-modal__pw-btn"
                  onClick={() => {
                    const chars = 'abcdefghjkmnpqrstuvwxyz23456789'
                    let pw = ''
                    for (let i = 0; i < 10; i++) {
                      pw += chars.charAt(Math.floor(Math.random() * chars.length))
                    }
                    setPassword(pw)
                    setShowPassword(true)
                  }}
                  title="Generate random password"
                  aria-label="Generate random password"
                  tabIndex={-1}
                >
                  <Sparkles size={14} aria-hidden="true" />
                </button>
              </div>
            </div>
            <span className="account-modal__hint">
              Used when authenticating SMTP (port 1025), IMAP (port 1143), and POP3 (port 1110) email clients.
            </span>
          </div>

          <div className="account-modal__field account-modal__field--checkbox">
            <label htmlFor="acc-catch-all" className="account-modal__checkbox-label">
              <input
                id="acc-catch-all"
                name="is_catch_all"
                type="checkbox"
                checked={isCatchAll}
                onChange={(e) => setIsCatchAll(e.target.checked)}
                disabled={isPending}
              />
              <span className="account-modal__checkbox-text">
                <strong>Set as Catch-All Account</strong>
                <span className="account-modal__hint">
                  If checked, any email sent to an unassigned address will route to this account&rsquo;s Inbox.
                </span>
              </span>
            </label>
          </div>

          <footer className="account-modal__footer">
            <button
              type="button"
              className="button button--ghost"
              onClick={onClose}
              disabled={isPending}
            >
              Cancel
            </button>
            <button
              type="submit"
              className="button button--primary"
              disabled={isPending || (!isEditing && !username.trim())}
            >
              {isPending ? (
                'Saving...'
              ) : (
                <>
                  <Check size={14} aria-hidden="true" />
                  <span>{isEditing ? 'Save Changes' : 'Create Account'}</span>
                </>
              )}
            </button>
          </footer>
        </form>
      </div>
    </div>
  )
}
