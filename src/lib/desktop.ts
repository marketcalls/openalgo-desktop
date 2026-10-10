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

import { invoke, isTauri } from '@tauri-apps/api/core'
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
 * How a broker's sign-in starts, as the local server reports it (`sign_in`
 * on GET /api/broker/configured and /auth/broker-config). The page keeps no
 * list of its own, so it cannot drift from the server's broker catalogue.
 *
 * - `redirect`: the broker's own login page. The web builds that address in
 *   the browser from the broker API key; the desktop never sends the key to
 *   the page (/auth/broker-config returns broker_api_key as null), so the
 *   local server builds the address, records the sign-in and redirects:
 *   GET /<broker>/initiate-oauth.
 * - `form`: the broker's in-app page, through GET /<broker>/callback as on
 *   the web.
 * - `saved_keys`: the saved keys are all the broker needs (the XTS brokers,
 *   Delta Exchange, Dhan Sandbox), so the page signs in with one action, as
 *   the web does on its first visit to /<broker>/callback.
 */
export type BrokerSignIn = 'redirect' | 'form' | 'saved_keys'

/** Where to send the browser to sign in to a redirect broker, or null. */
export function desktopBrokerLoginUrl(
  broker: string,
  signIn: BrokerSignIn | undefined
): string | null {
  if (!broker || signIn !== 'redirect') return null
  return `/${encodeURIComponent(broker)}/initiate-oauth`
}

/**
 * Sign in to a broker whose saved keys are all it needs: a POST to its
 * callback carrying the CSRF token (as the broker login form posts it) and
 * no login fields. Returns null when signed in, else the server's message
 * for the trader.
 */
export async function signInWithSavedKeys(broker: string): Promise<string | null> {
  const csrf = await csrfToken()
  if (!csrf) return 'Could not reach OpenAlgo. Try again.'
  const form = new FormData()
  form.append('csrf_token', csrf)
  try {
    const res = await fetch(`/${encodeURIComponent(broker)}/callback`, {
      method: 'POST',
      credentials: 'include',
      headers: { 'X-CSRFToken': csrf },
      body: form,
    })
    const body = await res.json().catch(() => null)
    if (res.ok && body?.status === 'success') return null
    return body?.message || 'Could not sign in to your broker. Try again.'
  } catch {
    return 'Could not reach OpenAlgo. Try again.'
  }
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
  /** How this broker's sign-in starts. */
  sign_in?: BrokerSignIn
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
 * The broker whose session is live in OpenAlgo, from a `/auth/session-status`
 * answer, or null when none is.
 */
export function liveBrokerOf(status: unknown): string | null {
  const s = status as { logged_in?: unknown; broker?: unknown } | null
  return s?.logged_in === true && typeof s.broker === 'string' && s.broker ? s.broker : null
}

/**
 * The broker whose session is live now. A switch to another broker ends it,
 * so the pages confirm first. When the server cannot be asked, `fallback`
 * (the active broker) is assumed live, so the trader is still asked.
 */
export async function fetchLiveBroker(fallback: string | null): Promise<string | null> {
  try {
    const res = await fetch('/auth/session-status', { credentials: 'include' })
    if (!res.ok) return fallback
    return liveBrokerOf(await res.json())
  } catch {
    return fallback
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

// ============================================================================
// MCP access tokens for AI clients (API key page; desktop only)
// ============================================================================

/** An MCP token as listed by GET /api/mcp/tokens (never the token itself). */
export interface McpToken {
  id: number
  name: string
  scope: 'read' | 'read_write'
  token_prefix: string
  created_at: string
  last_used_at: string | null
}

/** Ready-to-paste client configuration from the local server. */
export interface McpClientConfig {
  executable: string
  server_url: string
  mcp_url: string
  claude_desktop: unknown
  /** Carries `<MCP_TOKEN>`, never the token: a shell keeps a history. */
  claude_code: string
  claude_code_note?: string
}

async function csrfToken(): Promise<string | undefined> {
  return fetch('/auth/csrf-token', { credentials: 'include' })
    .then((r) => r.json())
    .then((b) => b?.csrf_token as string | undefined)
    .catch(() => undefined)
}

async function mcpWrite(url: string, method: 'POST' | 'DELETE', body?: unknown) {
  const csrf = await csrfToken()
  if (!csrf) throw new Error('Could not reach OpenAlgo. Try again.')
  const res = await fetch(url, {
    method,
    credentials: 'include',
    headers: { 'Content-Type': 'application/json', 'X-CSRFToken': csrf },
    body: body === undefined ? undefined : JSON.stringify(body),
  })
  const data = await res.json().catch(() => null)
  if (!res.ok) throw new Error(data?.message || 'The request failed. Try again.')
  return data
}

/** Live MCP tokens, newest first. */
export async function fetchMcpTokens(): Promise<McpToken[]> {
  const res = await fetch('/api/mcp/tokens', { credentials: 'include' })
  if (!res.ok) return []
  const body = await res.json().catch(() => null)
  return Array.isArray(body?.data) ? body.data : []
}

/** Create a token; the token and its client configuration come back once. */
export async function createMcpToken(
  name: string,
  scope: 'read' | 'read_write'
): Promise<{ token: string; client_config: McpClientConfig }> {
  const data = await mcpWrite('/api/mcp/tokens', 'POST', { name, scope })
  return { token: data.token, client_config: data.client_config }
}

/** Revoke a token: clients using it stop working at once. */
export async function revokeMcpToken(id: number): Promise<void> {
  await mcpWrite(`/api/mcp/tokens/${id}`, 'DELETE')
}

// ============================================================================
// Account reset (desktop window only)
// ============================================================================

/** What the desktop window answers to a reset request. */
export interface ResetAccountOutcome {
  status: 'reset' | 'cancelled'
  message: string
}

/**
 * Last-resort recovery when both the password and the authenticator are
 * lost. There is no web address for it: the desktop shell asks the trader to
 * confirm in a system dialog and only then resets, so nothing reaching the
 * local server over the network can wipe the account.
 */
export async function resetAccountFromDesktop(): Promise<ResetAccountOutcome> {
  if (!isDesktopShell()) {
    throw new Error('Open OpenAlgo Desktop on this computer to reset your account.')
  }
  try {
    return await invoke<ResetAccountOutcome>('reset_account')
  } catch (e) {
    const message =
      typeof e === 'object' && e !== null && 'message' in e ? String(e.message) : undefined
    throw new Error(message || 'Your account could not be reset. Restart OpenAlgo and try again.')
  }
}
