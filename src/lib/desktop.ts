/**
 * Everything the desktop app does differently from OpenAlgo web, in one place.
 *
 * The frontend is the web frontend carried over nearly verbatim, so that a web
 * change can be applied here by diff. The few things a desktop shell needs on
 * top live in this module, and each web file that uses it carries a single
 * "Desktop:" comment explaining why. Keep it small: business logic belongs in
 * the local server, which the app reaches over HTTP and Socket.IO exactly as
 * the web frontend reaches Flask.
 *
 * The same page can also be opened in an ordinary browser pointed at the local
 * server, so every helper here works outside the Tauri shell too.
 */

import { isTauri } from '@tauri-apps/api/core'
import { Server } from 'lucide-react'
import type { NavItem } from '@/config/navigation'

/** True when running inside the Tauri window rather than a plain browser tab. */
export function isDesktopShell(): boolean {
  try {
    return isTauri()
  } catch {
    return false
  }
}

/**
 * The market data feed address used when the server does not report one.
 *
 * Matches the shipped default (8765) and the development port (8766) the
 * maintainer uses so the desktop can run next to OpenAlgo web on one machine.
 */
export const DEFAULT_WEBSOCKET_URL = import.meta.env.DEV
  ? 'ws://127.0.0.1:8766'
  : 'ws://127.0.0.1:8765'

/**
 * Brokers whose sign-in is a redirect to the broker's own login page.
 *
 * The web builds that address in the browser from the broker API key. The
 * desktop never sends the key to the page (/auth/broker-config returns
 * broker_api_key as null), so the local server builds the address, records
 * the sign-in state and redirects: GET /<broker>/initiate-oauth.
 */
const SERVER_OAUTH_BROKERS = new Set([
  'aliceblue',
  'arrow',
  'compositedge',
  'dhan',
  'flattrade',
  'fyers',
  'hdfcsecurities',
  'hdfcsky',
  'iiflcapital',
  'paytm',
  'pocketful',
  'upstox',
  'zerodha',
])

/** Where to send the browser to sign in to an OAuth broker, or null. */
export function desktopBrokerLoginUrl(broker: string): string | null {
  if (!SERVER_OAUTH_BROKERS.has(broker)) return null
  return `/${encodeURIComponent(broker)}/initiate-oauth`
}

/** Profile menu entries that exist only in the desktop app. */
export const desktopProfileMenuItems: NavItem[] = [
  { href: '/settings/server', label: 'Server Settings', icon: Server },
]

/** An absolute http(s) address on a different origin than the app itself. */
export function isExternalHttpUrl(href: string, base: string = window.location.href): boolean {
  let url: URL
  try {
    url = new URL(href, base)
  } catch {
    return false
  }
  if (url.protocol !== 'http:' && url.protocol !== 'https:') return false
  return url.origin !== new URL(base).origin
}

/**
 * Open a web page in the user's default browser.
 *
 * Inside the Tauri window a target="_blank" link or window.open() has no
 * browser tab to land in, so the shell plugin hands the address to the OS.
 * In a plain browser this is an ordinary new tab.
 */
export async function openExternal(url: string): Promise<void> {
  if (isDesktopShell()) {
    const { open } = await import('@tauri-apps/plugin-shell')
    await open(url)
    return
  }
  window.open(url, '_blank', 'noopener,noreferrer')
}

/**
 * Save a same-origin download (an export endpoint) without opening a window.
 *
 * The web opens exports with window.open(url, '_blank') and lets the new tab
 * turn into a download. The desktop window cannot open tabs, so the same
 * request is made through a temporary download link instead; the server's
 * Content-Disposition header still names the file.
 */
function downloadInPlace(url: string): void {
  const link = document.createElement('a')
  link.href = url
  link.download = ''
  link.rel = 'noopener'
  link.style.display = 'none'
  document.body.appendChild(link)
  link.click()
  link.remove()
}

function anchorFromEvent(event: MouseEvent): HTMLAnchorElement | null {
  const target = event.target
  if (!(target instanceof Element)) return null
  return target.closest('a[href]') as HTMLAnchorElement | null
}

/**
 * Route external links and new-window requests through the desktop shell.
 *
 * Installed once at startup. Does nothing in a plain browser. Returns a
 * function that removes the handlers again (used by tests).
 */
export function installDesktopShellHandlers(): () => void {
  if (!isDesktopShell()) return () => {}

  // Bubble phase on window: React's own onClick handlers (and any
  // preventDefault they call) have already run by the time this fires.
  const onClick = (event: MouseEvent) => {
    if (event.defaultPrevented || event.button !== 0) return
    const anchor = anchorFromEvent(event)
    if (!anchor) return
    const href = anchor.getAttribute('href') ?? ''
    if (!isExternalHttpUrl(href)) return
    event.preventDefault()
    void openExternal(anchor.href)
  }
  window.addEventListener('click', onClick)

  const originalOpen = window.open
  window.open = (url?: string | URL, target?: string, features?: string) => {
    const href = url === undefined ? '' : String(url)
    if (href && isExternalHttpUrl(href)) {
      void openExternal(new URL(href, window.location.href).toString())
      return null
    }
    if (href && (target === '_blank' || target === undefined)) {
      downloadInPlace(href)
      return null
    }
    return originalOpen.call(window, url, target, features)
  }

  return () => {
    window.removeEventListener('click', onClick)
    window.open = originalOpen
  }
}

/**
 * Pages a signed-in trader can open before connecting a broker. The web
 * reads broker keys from `.env`, so it never needs this; the desktop has no
 * `.env`, so the broker is configured in Profile before the first broker
 * login, and Server Settings may be needed to free a port first.
 */
export const DESKTOP_PRE_BROKER_PATHS = ['/profile', '/settings/server'] as const

export function isPreBrokerPath(pathname: string): boolean {
  const p = pathname.replace(/\/+$/, '') || '/'
  return (DESKTOP_PRE_BROKER_PATHS as readonly string[]).includes(p)
}

/** Profile link that opens the Broker Configuration tab. */
export const DESKTOP_BROKER_SETUP_PATH = '/profile?tab=broker'

/** Initial Profile tab from `?tab=`, so the broker page can link to it. */
export function desktopInitialProfileTab(fallback: string): string {
  try {
    return new URLSearchParams(window.location.search).get('tab') || fallback
  } catch {
    return fallback
  }
}

/**
 * Origin the app is served from, used to build a broker redirect URL when
 * none is saved yet. The web defaults to port 5000; the desktop runs on the
 * port configured in Server Settings (5500 in development).
 */
export function desktopDefaultOrigin(): string {
  return window.location.origin || 'http://127.0.0.1:5000'
}

/**
 * Desktop: whether the Profile broker form shows a Client ID field for
 * `broker`. The list comes from GET /api/broker/credentials
 * (`client_id_brokers`): brokers whose sign-in is bound to a client id the
 * trader enters separately.
 */
export function brokerNeedsClientId(
  broker: string | undefined,
  clientIdBrokers: readonly string[] | undefined
): boolean {
  return (
    Boolean(broker) && Array.isArray(clientIdBrokers) && clientIdBrokers.includes(broker as string)
  )
}

/** A broker with saved keys, from GET /api/broker/configured. */
export interface ConfiguredBroker {
  name: string
  active: boolean
}

/** Brokers the trader has configured, so the broker page can switch. */
export async function fetchConfiguredBrokers(): Promise<ConfiguredBroker[]> {
  try {
    const res = await fetch('/api/broker/configured', { credentials: 'include' })
    if (!res.ok) return []
    const body = await res.json()
    return Array.isArray(body?.data?.brokers) ? body.data.brokers : []
  } catch {
    return []
  }
}

/**
 * Make another configured broker the active one. Keys are stored per broker,
 * so only the redirect URL changes; the server ends any live session of the
 * previous broker first. Returns the server's message on refusal.
 */
export async function switchActiveBroker(broker: string): Promise<string | null> {
  const csrf = await fetch('/auth/csrf-token', { credentials: 'include' })
    .then((r) => r.json())
    .then((b) => b?.csrf_token as string | undefined)
    .catch(() => undefined)
  if (!csrf) return 'Could not reach OpenAlgo. Try again.'
  const form = new FormData()
  form.append('redirect_url', `${desktopDefaultOrigin()}/${broker}/callback`)
  const res = await fetch('/api/broker/credentials', {
    method: 'POST',
    credentials: 'include',
    headers: { 'X-CSRFToken': csrf },
    body: form,
  })
  if (res.ok) return null
  const body = await res.json().catch(() => null)
  return body?.message || 'Could not switch the broker. Try again.'
}
