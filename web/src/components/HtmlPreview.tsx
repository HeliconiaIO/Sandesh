/**
 * Renders untrusted email HTML.
 *
 * This is the application's most important security boundary (§74). Email HTML
 * is attacker-controlled: it arrives over SMTP from anyone who can reach the
 * port, and Sandesh's whole job is to show it to a developer.
 *
 * ```text
 * Sandesh origin (privileged: API, event stream, clipboard)
 *        │
 *        │  srcdoc + sandbox — no shared origin, no script execution
 *        ▼
 * iframe (opaque origin)
 *        │
 *        ▼
 * untrusted email HTML
 * ```
 *
 * The `sandbox` attribute is deliberately EMPTY. An empty sandbox denies
 * everything the spec can deny: scripts, forms, popups, top-level navigation,
 * pointer lock, and — critically — same-origin access. Two flags are the ones
 * that matter and neither is present:
 *
 * * `allow-scripts` would let a message run JavaScript. Even in an opaque
 *   origin that is a needless capability for *displaying* mail.
 * * `allow-same-origin` would place the message in Sandesh's own origin, where
 *   it could read the API, the event stream and local storage.
 *
 * Together they would be equivalent to no sandbox at all, which is why they are
 * never combined. Do not add either to make a message render "properly": a
 * message that needs script to render is a message a developer needs to SEE
 * failing, and the Raw and MIME tabs already show exactly what it contains.
 *
 * `srcdoc` rather than a blob or data URL keeps the content out of any URL the
 * browser might treat as navigable, and a restrictive CSP inside the document
 * itself blocks network access, so a tracking pixel cannot phone home just by
 * being displayed.
 */

import { useMemo } from 'react'

export function getEmailNaturalWidth(html: string): number {
  if (!html) return 0
  // If the email uses responsive media queries, it is designed to reflow on mobile
  if (/@media[^{]+(max-width|min-width)/i.test(html)) {
    return 0
  }
  let maxWidth = 0
  // Match width attribute on table, td, div
  const widthAttrRegex = /<(?:table|div|td)[^>]*\bwidth=["']?(\d+)/gi
  let match: RegExpExecArray | null
  while ((match = widthAttrRegex.exec(html)) !== null) {
    const val = match[1]
    if (val) {
      const w = parseInt(val, 10)
      if (w >= 320 && w <= 1400) {
        maxWidth = Math.max(maxWidth, w)
      }
    }
  }
  // Match style width / min-width / max-width
  const styleWidthRegex = /(?:min-|max-)?width:\s*(\d+)px/gi
  while ((match = styleWidthRegex.exec(html)) !== null) {
    const val = match[1]
    if (val) {
      const w = parseInt(val, 10)
      if (w >= 320 && w <= 1400) {
        maxWidth = Math.max(maxWidth, w)
      }
    }
  }
  return maxWidth > 0 ? maxWidth + 40 : 0
}

interface HtmlPreviewProps {
  html: string
  /** Whether to allow remote images. Off by default: loading them leaks that
   *  the message was opened, which a mail-testing tool must not do silently. */
  allowRemote: boolean
  title: string
  viewport?: 'desktop' | 'tablet' | 'mobile'
  orientation?: 'portrait' | 'landscape'
}

/**
 * The policy applied INSIDE the frame, defending even if the sandbox were
 * somehow relaxed. `default-src 'none'` denies every fetch the document could
 * attempt; only inline styles and (optionally) images are permitted.
 */
function contentPolicy(allowRemote: boolean): string {
  const img = allowRemote ? "img-src data: https: http:" : "img-src data:"
  return [
    "default-src 'none'",
    img,
    "style-src 'unsafe-inline'",
    "font-src data:",
    // Belt and braces: the sandbox already blocks these.
    "script-src 'none'",
    "object-src 'none'",
    "frame-src 'none'",
    "form-action 'none'",
    "base-uri 'none'",
  ].join('; ')
}

export function HtmlPreview({
  html,
  allowRemote,
  title,
  viewport = 'desktop',
  orientation = 'portrait',
}: HtmlPreviewProps) {
  const naturalWidth = useMemo(() => getEmailNaturalWidth(html), [html])

  const { scale, availableWidth, availableHeight } = useMemo(() => {
    if (viewport === 'desktop' || !naturalWidth) {
      return { scale: 1, availableWidth: 0, availableHeight: 0 }
    }
    const availW =
      viewport === 'mobile'
        ? orientation === 'landscape'
          ? 740
          : 366
        : orientation === 'landscape'
          ? 932
          : 740
    const availH =
      viewport === 'mobile'
        ? orientation === 'landscape'
          ? 356
          : 740
        : orientation === 'landscape'
          ? 740
          : 932

    if (naturalWidth > availW) {
      return {
        scale: availW / naturalWidth,
        availableWidth: availW,
        availableHeight: availH,
      }
    }
    return { scale: 1, availableWidth: availW, availableHeight: availH }
  }, [viewport, orientation, naturalWidth])

  const document = useMemo(() => {
    // The CSP goes in a meta tag because `srcdoc` carries no HTTP headers.
    // Links open in a new tab rather than navigating the frame, and the base
    // target is set explicitly so a message cannot retarget the application.
    return `<!doctype html>
<html>
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<meta http-equiv="Content-Security-Policy" content="${contentPolicy(allowRemote)}">
<base target="_blank">
<style>
  html {
    color-scheme: light;
    -webkit-text-size-adjust: 100%;
    text-size-adjust: 100%;
  }
  html, body {
    margin: 0;
    padding: 0;
    overflow-x: hidden;
  }
  body {
    background: #ffffff;
    color: #1f2328;
    font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
    font-size: 14px;
    line-height: 1.5;
    word-break: break-word;
  }
  img {
    max-width: 100%;
    height: auto;
  }
  table {
    max-width: 100%;
  }
</style>
</head>
<body>${html}</body>
</html>`
  }, [html, allowRemote])

  const iframeStyle: React.CSSProperties =
    scale < 1
      ? {
          width: `${Math.round(availableWidth / scale)}px`,
          height: `${Math.round(availableHeight / scale)}px`,
          transform: `scale(${scale})`,
          transformOrigin: 'top left',
          border: 'none',
          background: '#ffffff',
          display: 'block',
        }
      : {
          width: '100%',
          height: '100%',
          border: 'none',
          background: '#ffffff',
          display: 'block',
        }

  return (
    <iframe
      // An empty sandbox. See the module comment before changing this.
      sandbox=""
      srcDoc={document}
      title={title}
      referrerPolicy="no-referrer"
      loading="lazy"
      style={iframeStyle}
    />
  )
}
