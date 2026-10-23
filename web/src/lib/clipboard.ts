/**
 * Clipboard access (§61).
 *
 * `navigator.clipboard` needs a secure context, which `http://localhost` is but
 * `http://192.168.x.x` is not — a real case, since a developer may open Sandesh
 * from another machine. The fallback keeps copy working there rather than
 * failing silently.
 */
export async function copyText(text: string): Promise<boolean> {
  try {
    if (navigator.clipboard && window.isSecureContext) {
      await navigator.clipboard.writeText(text)
      return true
    }
  } catch {
    // Permission denied or unavailable; try the fallback below.
  }

  try {
    const area = document.createElement('textarea')
    area.value = text
    area.setAttribute('readonly', '')
    area.style.position = 'fixed'
    area.style.opacity = '0'
    document.body.appendChild(area)
    area.select()
    const copied = document.execCommand('copy')
    document.body.removeChild(area)
    return copied
  } catch {
    return false
  }
}
