import { useEffect, useState } from 'react'
import { Check, Copy } from 'lucide-react'

import { copyText } from '../lib/clipboard'

/**
 * Copies a value and confirms it (§61).
 *
 * The confirmation is the icon swap, not a toast: copying is frequent and
 * low-stakes, and a toast for every copy is noise (§51).
 */
export function CopyButton({
  value,
  label,
  className,
}: {
  value: string
  label: string
  className?: string
}) {
  const [copied, setCopied] = useState(false)

  useEffect(() => {
    if (!copied) return
    const timer = setTimeout(() => setCopied(false), 1400)
    return () => clearTimeout(timer)
  }, [copied])

  return (
    <button
      type="button"
      className={className ?? 'icon-button'}
      onClick={async () => {
        if (await copyText(value)) setCopied(true)
      }}
      // The label names what is copied, so a screen reader user knows which of
      // several copy buttons on a page they have landed on.
      aria-label={copied ? `${label} copied` : `Copy ${label}`}
      title={`Copy ${label}`}
    >
      {copied ? (
        <Check size={13} aria-hidden="true" className="icon-button__ok" />
      ) : (
        <Copy size={13} aria-hidden="true" />
      )}
    </button>
  )
}
