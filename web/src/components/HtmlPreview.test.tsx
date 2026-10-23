/**
 * The HTML isolation boundary (§104, §107).
 *
 * These assert the properties that keep attacker-controlled email HTML from
 * reaching the application's origin. They are deliberately strict: a change
 * that adds `allow-scripts` or `allow-same-origin` to make some message render
 * "better" must fail here loudly rather than pass review quietly.
 */

import { describe, expect, it } from 'vitest'
import { render } from '@testing-library/react'

import { HtmlPreview } from './HtmlPreview'

function frameOf(container: HTMLElement): HTMLIFrameElement {
  const frame = container.querySelector('iframe')
  if (!frame) throw new Error('the preview must render an iframe')
  return frame
}

describe('HtmlPreview', () => {
  it('renders email HTML inside an iframe, never in the app DOM', () => {
    // The single most important property: the markup must not be adopted into
    // the application's document, where it would share this origin.
    const { container } = render(
      <HtmlPreview html="<h1 id='pwned'>hello</h1>" allowRemote={false} title="Body" />,
    )

    expect(frameOf(container)).toBeInTheDocument()
    expect(container.querySelector('#pwned')).toBeNull()
  })

  it('applies a fully restrictive sandbox', () => {
    const { container } = render(
      <HtmlPreview html="<p>hi</p>" allowRemote={false} title="Body" />,
    )

    // An empty string denies every sandbox capability there is.
    expect(frameOf(container).getAttribute('sandbox')).toBe('')
  })

  it('never grants script execution or same-origin access', () => {
    const { container } = render(
      <HtmlPreview html="<script>parent.location='/'</script>" allowRemote={false} title="Body" />,
    )

    const sandbox = frameOf(container).getAttribute('sandbox') ?? ''
    expect(sandbox).not.toContain('allow-scripts')
    expect(sandbox).not.toContain('allow-same-origin')
  })

  it('carries a content policy that blocks script and network access', () => {
    const { container } = render(
      <HtmlPreview html="<p>hi</p>" allowRemote={false} title="Body" />,
    )

    const doc = frameOf(container).getAttribute('srcdoc') ?? ''
    expect(doc).toContain("default-src 'none'")
    expect(doc).toContain("script-src 'none'")
    expect(doc).toContain("object-src 'none'")
  })

  it('blocks remote images until the reader asks for them', () => {
    // Loading a remote image silently confirms the address was read, which a
    // mail-testing tool must not do on the user's behalf.
    const { container } = render(
      <HtmlPreview html="<img src='https://tracker.test/p.gif'>" allowRemote={false} title="Body" />,
    )

    const doc = frameOf(container).getAttribute('srcdoc') ?? ''
    expect(doc).toContain('img-src data:')
    expect(doc).not.toContain('img-src data: https:')
  })

  it('permits remote images only once explicitly allowed', () => {
    const { container } = render(
      <HtmlPreview html="<img src='https://cdn.test/p.png'>" allowRemote title="Body" />,
    )

    expect(frameOf(container).getAttribute('srcdoc') ?? '').toContain('img-src data: https:')
  })

  it('sends no referrer to anything the message loads', () => {
    const { container } = render(
      <HtmlPreview html="<p>hi</p>" allowRemote title="Body" />,
    )

    expect(frameOf(container).getAttribute('referrerPolicy')).toBe('no-referrer')
  })

  it('does not execute SVG content in the application origin', () => {
    // SVG can carry script, so it is subject to the same isolation as HTML.
    const svg = '<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>'
    const { container } = render(<HtmlPreview html={svg} allowRemote={false} title="Body" />)

    expect(container.querySelector('svg')).toBeNull()
    expect(container.querySelector('script')).toBeNull()
    expect(frameOf(container).getAttribute('sandbox')).toBe('')
  })

  it('includes viewport meta tag and auto-fits fixed-width emails for mobile screen', () => {
    const emailHtml = '<table width="600"><tr><td>Hello Mobile</td></tr></table>'
    const { container } = render(
      <HtmlPreview html={emailHtml} allowRemote={false} title="Body" viewport="mobile" orientation="portrait" />,
    )

    const frame = frameOf(container)
    const doc = frame.getAttribute('srcdoc') ?? ''
    expect(doc).toContain('<meta name="viewport" content="width=device-width, initial-scale=1.0">')
    expect(doc).toContain('overflow-x: hidden')
    // The frame scales down proportionally so the full 640px (600+40) fits the 366px screen
    expect(frame.style.transform).toContain('scale')
  })
})
