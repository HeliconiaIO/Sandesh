/**
 * Import and export (Phase 8 A5).
 *
 * The view's single job: move a corpus in or out, and make the guarantee that
 * makes it safe — an export re-imports byte-identically — legible rather than
 * merely claimed.
 *
 * That guarantee is why the two halves sit side by side as one reversible pair
 * instead of on separate screens. The arrow between them is the view's
 * signature: it is the round trip, drawn. Everything else is deliberately
 * quiet, because the interesting moment here is the result of an import, not
 * the form that started it.
 *
 * `mboxrd` is named in the copy rather than hidden. Anyone moving a mail corpus
 * around has been bitten by an `mboxo` writer silently accumulating `>` on
 * their `From ` lines, and naming the variant is the fastest way to say this
 * one does not.
 */

import { useMemo, useRef, useState } from 'react'
import { ArrowLeftRight, Download, FolderInput, FolderOutput, Upload } from 'lucide-react'

import { ApiError, type ImportResponse } from '../api/client'
import { transfer } from '../api/resources'
import { Select, type SelectItem } from '../components/Select'
import { useAccounts, useImportMessages, useMailboxes } from '../hooks/queries'
import './TransferRoute.css'

/** The container formats the server accepts in both directions. */
type Format = 'mbox' | 'eml'

export function TransferRoute() {
  const accountsQuery = useAccounts()
  const mailboxesQuery = useMailboxes()
  const importMessages = useImportMessages()

  const [selectedAccountId, setSelectedAccountId] = useState('')
  const [selectedMailboxId, setSelectedMailboxId] = useState('')
  const [importMailboxId, setImportMailboxId] = useState('')
  const [importFormat, setImportFormat] = useState<Format>('mbox')
  const [result, setResult] = useState<ImportResponse | null>(null)
  const [error, setError] = useState<string | null>(null)
  const fileInput = useRef<HTMLInputElement>(null)

  const accounts = accountsQuery.data?.items ?? []
  const mailboxes = mailboxesQuery.data?.items ?? []

  // Group mailboxes by account
  const mailboxesByAccount = new Map<string, typeof mailboxes>()
  for (const mb of mailboxes) {
    const list = mailboxesByAccount.get(mb.account_id) ?? []
    list.push(mb)
    mailboxesByAccount.set(mb.account_id, list)
  }

  const selectedAccount = accounts.find((a) => a.id === selectedAccountId)
  const selectedMailbox = mailboxes.find((m) => m.id === selectedMailboxId)

  // Filtered mailboxes for the selected account (if an account is picked)
  const availableMailboxes = selectedAccountId
    ? (mailboxesByAccount.get(selectedAccountId) ?? [])
    : mailboxes

  const accountOptions: SelectItem[] = useMemo(
    () => [
      { value: '', label: 'Every account' },
      ...accounts.map((account) => {
        const email = account.addresses?.[0]
        return {
          value: account.id,
          label: `${account.username.toUpperCase()}${
            account.is_catch_all ? ' (Catch-all)' : email ? ` (${email})` : ''
          }`,
        }
      }),
    ],
    [accounts],
  )

  const mailboxOptions: SelectItem[] = useMemo(() => {
    if (accounts.length > 1) {
      if (selectedAccountId) {
        return [
          {
            value: '',
            label: `All folders in ${selectedAccount?.username.toUpperCase() ?? 'account'}`,
          },
          ...availableMailboxes.map((mailbox) => ({
            value: mailbox.id,
            label: `${mailbox.name}${mailbox.total ? ` (${mailbox.total})` : ''}`,
          })),
        ]
      }
      const groups: SelectItem[] = [
        { value: '', label: 'Every mailbox across all accounts' },
      ]
      for (const acc of accounts) {
        const accMailboxes = mailboxesByAccount.get(acc.id) ?? []
        if (accMailboxes.length === 0) continue
        groups.push({
          label: `${acc.username.toUpperCase()}${acc.is_catch_all ? ' (Catch-all)' : ''}`,
          options: accMailboxes.map((mailbox) => ({
            value: mailbox.id,
            label: `${mailbox.name}${mailbox.total ? ` (${mailbox.total})` : ''}`,
          })),
        })
      }
      return groups
    }

    return [
      { value: '', label: 'Every mailbox' },
      ...mailboxes.map((mailbox) => ({
        value: mailbox.id,
        label: `${mailbox.name}${mailbox.total ? ` (${mailbox.total})` : ''}`,
      })),
    ]
  }, [accounts, selectedAccountId, selectedAccount, availableMailboxes, mailboxesByAccount, mailboxes])

  const formatOptions: SelectItem[] = useMemo(
    () => [
      { value: 'mbox', label: 'mbox — many messages' },
      { value: 'eml', label: '.eml — one message' },
    ],
    [],
  )

  const importMailboxOptions: SelectItem[] = useMemo(() => {
    const defaultOption = { value: '', label: 'Auto-deliver to recipient Inbox (Default)' }
    if (accounts.length > 1) {
      const groups: SelectItem[] = [defaultOption]
      for (const acc of accounts) {
        const accMailboxes = mailboxesByAccount.get(acc.id) ?? []
        if (accMailboxes.length === 0) continue
        groups.push({
          label: `${acc.username.toUpperCase()}${acc.is_catch_all ? ' (Catch-all)' : ''}`,
          options: accMailboxes.map((mailbox) => ({
            value: mailbox.id,
            label: mailbox.name,
          })),
        })
      }
      return groups
    }
    return [
      defaultOption,
      ...mailboxes.map((mailbox) => ({
        value: mailbox.id,
        label: mailbox.name,
      })),
    ]
  }, [accounts, mailboxesByAccount, mailboxes])

  const handleAccountChange = (accountId: string) => {
    setSelectedAccountId(accountId)
    setSelectedMailboxId('')
  }

  const handleMailboxChange = (mailboxId: string) => {
    setSelectedMailboxId(mailboxId)
    if (mailboxId) {
      const mb = mailboxes.find((m) => m.id === mailboxId)
      if (mb && mb.account_id !== selectedAccountId) {
        setSelectedAccountId(mb.account_id)
      }
    }
  }

  const handleImport = async (file: File) => {
    setResult(null)
    setError(null)

    try {
      const response = await importMessages.mutateAsync({
        file,
        format: importFormat,
        mailboxId: importMailboxId || undefined,
      })
      setResult(response)
    } catch (err: unknown) {
      // The server explains refusals precisely — an mbox with no separator
      // line, an unsupported format — so pass its words through rather than
      // replacing them with a generic failure.
      setError(
        err instanceof ApiError
          ? err.detail || err.message
          : 'The file could not be imported.',
      )
    } finally {
      // Clear the input so re-selecting the same file fires a change event.
      if (fileInput.current) fileInput.current.value = ''
    }
  }

  const exportHref = transfer.exportUrl({
    format: 'mbox',
    accountId: selectedMailboxId ? undefined : (selectedAccountId || undefined),
    mailboxId: selectedMailboxId || undefined,
  })

  const downloadFilename = selectedMailbox
    ? `${selectedMailbox.name.toLowerCase()}.mbox`
    : selectedAccount
      ? `${selectedAccount.username.toLowerCase()}-all-mailboxes.mbox`
      : 'sandesh-all-mailboxes.mbox'

  const downloadLabel = selectedMailbox
    ? `Download mbox (${selectedMailbox.name})`
    : selectedAccount
      ? `Download mbox (${selectedAccount.username.toUpperCase()})`
      : 'Download mbox'

  return (
    <section className="transfer" aria-label="Import and export">
      <header className="transfer__bar">
        <h1 className="transfer__title">Import and export</h1>
        <p className="transfer__subtitle">
          Move messages in and out as <code>mbox</code>. Exports use{' '}
          <code>mboxrd</code> framing, so a file written here re-imports exactly
          as it left — <code>From </code> lines and all.
        </p>
      </header>

      <div className="transfer__body">
        <div className="transfer__pair">
          <section className="transfer__card" aria-labelledby="transfer-export">
            <div className="transfer__card-head">
              <FolderOutput size={15} aria-hidden="true" />
              <h2 className="transfer__card-title" id="transfer-export">
                Export
              </h2>
            </div>
            <p className="transfer__card-note">
              Downloads messages as an mbox file, framed and ready for re-import or external clients.
            </p>

            {accounts.length > 1 ? (
              <>
                <label className="transfer__label" htmlFor="transfer-account">
                  Account
                </label>
                <Select
                  id="transfer-account"
                  value={selectedAccountId}
                  onChange={handleAccountChange}
                  options={accountOptions}
                  fullWidth
                />
              </>
            ) : null}

            <label className="transfer__label" htmlFor="transfer-mailbox">
              Mailbox
            </label>
            <Select
              id="transfer-mailbox"
              value={selectedMailboxId}
              onChange={handleMailboxChange}
              options={mailboxOptions}
              fullWidth
            />

            <a
              className="button button--primary transfer__action"
              href={exportHref}
              download={downloadFilename}
            >
              <Download size={13} aria-hidden="true" />
              {downloadLabel}
            </a>
          </section>

          {/* The signature: the round trip, drawn. The two cards are one
              reversible operation, and this is the claim that makes them so. */}
          <div className="transfer__link" aria-hidden="true">
            <span className="transfer__link-rule" />
            <span className="transfer__link-badge">
              <ArrowLeftRight size={13} />
            </span>
            <span className="transfer__link-rule" />
          </div>

          <section className="transfer__card" aria-labelledby="transfer-import">
            <div className="transfer__card-head">
              <FolderInput size={15} aria-hidden="true" />
              <h2 className="transfer__card-title" id="transfer-import">
                Import
              </h2>
            </div>
            <p className="transfer__card-note">
              Reads an mbox file, or a single message, into storage.
            </p>

            <label className="transfer__label" htmlFor="transfer-format">
              Format
            </label>
            <Select
              id="transfer-format"
              value={importFormat}
              onChange={(val) => setImportFormat(val as Format)}
              options={formatOptions}
              fullWidth
            />

            <label className="transfer__label" htmlFor="import-mailbox">
              Destination Mailbox
            </label>
            <Select
              id="import-mailbox"
              value={importMailboxId}
              onChange={setImportMailboxId}
              options={importMailboxOptions}
              fullWidth
            />

            <input
              ref={fileInput}
              id="transfer-file"
              className="transfer__file"
              type="file"
              accept={importFormat === 'mbox' ? '.mbox,application/mbox' : '.eml,message/rfc822'}
              onChange={(event) => {
                const file = event.target.files?.[0]
                if (file) void handleImport(file)
              }}
            />
            <label className="button transfer__action" htmlFor="transfer-file">
              <Upload size={13} aria-hidden="true" />
              {importMessages.isPending ? 'Importing…' : 'Choose a file'}
            </label>
          </section>
        </div>

        {error ? (
          <p className="transfer__result transfer__result--error" role="alert">
            {error}
          </p>
        ) : null}

        {result ? (
          <p className="transfer__result transfer__result--ok" role="status">
            Imported {result.imported}{' '}
            {result.imported === 1 ? 'message' : 'messages'}
            {result.parsed !== result.imported
              ? ` of ${result.parsed} found in the file`
              : ''}
            {importMailboxId
              ? ` into ${mailboxes.find((m) => m.id === importMailboxId)?.name ?? 'selected mailbox'}`
              : ''}
            .
          </p>
        ) : null}
      </div>
    </section>
  )
}
