import '@fontsource-variable/inter'
import '@fontsource-variable/jetbrains-mono'
import '@fontsource-variable/space-grotesk'
import logo from '../src/assets/sandesh-mark-dark.png'
import overview from '../../docs/images/overview-page.png'
import inspector from '../../docs/images/message-inspector-tabs.png'
import traces from '../../docs/images/traces-view-split.png'
import './site.css'
import './site-glass.css'

// Signature sheen on the readout follows a fine pointer.
const readout = document.querySelector<HTMLElement>('.hero__readout')
if (readout && window.matchMedia('(pointer: fine) and (prefers-reduced-motion: no-preference)').matches) {
  readout.addEventListener('pointermove', (event) => {
    const box = readout.getBoundingClientRect()
    readout.style.setProperty('--sheen-x', `${event.clientX - box.left}px`)
    readout.style.setProperty('--sheen-y', `${event.clientY - box.top}px`)
  })
}

const images: Record<string, string> = { mark: logo, overview, inspector, traces }

for (const image of document.querySelectorAll<HTMLImageElement>('[data-image]')) {
  const asset = image.dataset.image
  if (asset && images[asset]) image.src = images[asset]
}

const snippets: Record<string, string> = {
  docker: [
    'docker run -d \\',
    '  -p 1025:1025 -p 1080:1080 \\',
    '  -p 1143:1143 -p 1110:1110 \\',
    '  -v sandesh-data:/data \\',
    '  ghcr.io/heliconiaio/sandesh:latest',
  ].join('\n'),
  python: [
    'from email.message import EmailMessage',
    'import smtplib',
    '',
    'message = EmailMessage()',
    'message["From"] = "app@example.test"',
    'message["To"] = "inbox@example.test"',
    'message["Subject"] = "A local test"',
    'message.set_content("It worked.")',
    '',
    'with smtplib.SMTP("127.0.0.1", 1025) as smtp:',
    '    smtp.send_message(message)',
  ].join('\n'),
  node: [
    'import nodemailer from "nodemailer";',
    '',
    'const mailer = nodemailer.createTransport({',
    '  host: "127.0.0.1",',
    '  port: 1025,',
    '  secure: false,',
    '});',
    '',
    'await mailer.sendMail({',
    '  from: "app@example.test",',
    '  to: "inbox@example.test",',
    '  subject: "A local test",',
    '  text: "It worked.",',
    '});',
  ].join('\n'),
}

const snippetTabs = [...document.querySelectorAll<HTMLButtonElement>('[data-snippet]')]
const snippetCode = document.querySelector<HTMLElement>('#quickstart-command')
const snippetPanel = document.querySelector<HTMLElement>('#quickstart-panel')
const snippetTitle = document.querySelector<HTMLElement>('[data-snippet-title]')
const copyStatus = document.querySelector<HTMLElement>('.copy-status')

function activateSnippet(tab: HTMLButtonElement, moveFocus = false) {
  const name = tab.dataset.snippet
  if (!name || !snippets[name] || !snippetCode || !snippetPanel) return

  for (const candidate of snippetTabs) {
    const active = candidate === tab
    candidate.setAttribute('aria-selected', String(active))
    candidate.tabIndex = active ? 0 : -1
  }

  snippetCode.textContent = snippets[name]
  snippetPanel.setAttribute('aria-labelledby', tab.id)
  if (snippetTitle) snippetTitle.textContent = tab.textContent ?? ''
  if (copyStatus) copyStatus.textContent = ''
  if (moveFocus) tab.focus()
}

for (const [index, tab] of snippetTabs.entries()) {
  tab.addEventListener('click', () => activateSnippet(tab))
  tab.addEventListener('keydown', (event) => {
    let nextIndex: number | undefined
    if (event.key === 'ArrowRight') nextIndex = (index + 1) % snippetTabs.length
    if (event.key === 'ArrowLeft') nextIndex = (index - 1 + snippetTabs.length) % snippetTabs.length
    if (event.key === 'Home') nextIndex = 0
    if (event.key === 'End') nextIndex = snippetTabs.length - 1
    if (nextIndex === undefined) return

    event.preventDefault()
    const nextTab = snippetTabs[nextIndex]
    if (nextTab) activateSnippet(nextTab, true)
  })
}

const menuToggle = document.querySelector<HTMLButtonElement>('.menu-toggle')
const siteNav = document.querySelector<HTMLElement>('#site-nav')

function closeMenu() {
  siteNav?.classList.remove('site-nav--open')
  menuToggle?.setAttribute('aria-expanded', 'false')
  menuToggle?.setAttribute('aria-label', 'Open navigation')
}

menuToggle?.addEventListener('click', () => {
  const isOpen = siteNav?.classList.toggle('site-nav--open') ?? false
  menuToggle.setAttribute('aria-expanded', String(isOpen))
  menuToggle.setAttribute('aria-label', isOpen ? 'Close navigation' : 'Open navigation')
})

siteNav?.querySelectorAll('a').forEach((link) => link.addEventListener('click', closeMenu))
document.addEventListener('keydown', (event) => {
  if (event.key === 'Escape') closeMenu()
})

const copyButton = document.querySelector<HTMLButtonElement>('[data-copy-command]')
const command = document.querySelector<HTMLElement>('#quickstart-command')

async function copyText(value: string, feedback: HTMLElement | null, idleLabel = '') {
  if (!navigator.clipboard) {
    if (feedback) feedback.textContent = 'Clipboard unavailable.'
    return false
  }

  try {
    await navigator.clipboard.writeText(value)
    if (feedback) feedback.textContent = 'Copied.'
    window.setTimeout(() => {
      if (feedback) feedback.textContent = idleLabel
    }, 1800)
    return true
  } catch {
    if (feedback) feedback.textContent = 'Clipboard unavailable.'
    return false
  }
}

for (const button of document.querySelectorAll<HTMLButtonElement>('[data-copy-value]')) {
  button.addEventListener('click', async () => {
    const value = button.dataset.copyValue
    const feedback = button.querySelector<HTMLElement>('[data-copy-feedback]')
    if (value) await copyText(value, feedback, 'Copy')
  })
}

copyButton?.addEventListener('click', async () => {
  if (command) await copyText(command.textContent ?? '', copyStatus)
})

const revealTargets = document.querySelectorAll<HTMLElement>('[data-reveal]')
const reduceMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches

if (!reduceMotion && 'IntersectionObserver' in window) {
  document.documentElement.classList.add('motion-ready')
  const observer = new IntersectionObserver((entries, currentObserver) => {
    for (const entry of entries) {
      if (!entry.isIntersecting) continue
      entry.target.classList.add('is-visible')
      currentObserver.unobserve(entry.target)
    }
  }, { threshold: 0.12, rootMargin: '0px 0px -36px 0px' })

  for (const [index, target] of [...revealTargets].entries()) {
    target.style.setProperty('--reveal-delay', `${(index % 3) * 55}ms`)
    observer.observe(target)
  }
} else {
  revealTargets.forEach((target) => target.classList.add('is-visible'))
}