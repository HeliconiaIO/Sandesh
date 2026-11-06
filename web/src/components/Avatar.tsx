/**
 * A sender's initial on a neutral disc (styles in `styles/components.css`).
 *
 * Decorative: the sender's name is always rendered as text beside it, so the
 * avatar is hidden from assistive technology rather than announced twice.
 */
export function Avatar({
  name,
  size = 'md',
}: {
  name: string | null | undefined
  size?: 'sm' | 'md' | 'lg'
}) {
  const initial = (name ?? '').match(/[\p{L}\p{N}]/u)?.[0] ?? '?'
  const sizeClass = size === 'md' ? '' : ` avatar--${size}`
  return (
    <span className={`avatar${sizeClass}`} aria-hidden="true">
      {initial}
    </span>
  )
}
