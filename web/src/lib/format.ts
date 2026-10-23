/** Presentation helpers. No domain decisions live here — those belong in Rust. */

/** Bytes as a compact human size. */
export function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`
}

/** A timestamp as a short relative age, for list rows. */
export function formatAge(iso: string): string {
  const then = new Date(iso).getTime()
  if (Number.isNaN(then)) return '—'

  const seconds = Math.max(0, Math.floor((Date.now() - then) / 1000))
  if (seconds < 60) return `${seconds}s`
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m`
  if (seconds < 86_400) return `${Math.floor(seconds / 3600)}h`
  if (seconds < 604_800) return `${Math.floor(seconds / 86_400)}d`

  return new Date(then).toLocaleDateString(undefined, { month: 'short', day: 'numeric' })
}

/** A timestamp in full, for the message header. */
export function formatFull(iso: string): string {
  const date = new Date(iso)
  if (Number.isNaN(date.getTime())) return iso
  return date.toLocaleString(undefined, {
    year: 'numeric',
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  })
}

/** An address as `Name <addr>`, or just the address when unnamed. */
export function formatAddress(
  address: { address: string; display_name?: string | null } | null | undefined,
): string {
  if (!address) return '—'
  return address.display_name ? `${address.display_name} <${address.address}>` : address.address
}

/** The short form used in dense list rows. */
export function formatAddressShort(
  address: { address: string; display_name?: string | null } | null | undefined,
): string {
  if (!address) return '—'
  return address.display_name || address.address
}

/** IMAP flag names are backslash-prefixed; strip it for display. */
export function flagLabel(flag: string): string {
  return flag.startsWith('\\') ? flag.slice(1) : flag
}
