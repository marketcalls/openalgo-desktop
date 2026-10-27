/**
 * The one guard on a provider endpoint the trader types (D-028).
 *
 * The add dialog and the edit dialog both run it, and the server runs the
 * same steps (`agent::providers::base_url`) against the same cases
 * (`src-tauri/tests/fixtures/agent/base_url_cases.json`), so the three cannot
 * disagree. On the web they did: the add dialog missed an IPv4-mapped IPv6
 * spelling of the metadata address and the edit dialog checked nothing.
 *
 * OpenAlgo makes requests to this address, so it is checked before it is
 * saved: http or https only, no username or password in it, a host, and never
 * the cloud metadata service. A private or loopback host is allowed on
 * purpose: a model on the trader's own computer is the point of these kinds.
 * The host is read the way the URL standard reads it, which is how the request
 * will read it, so `http://2852039166/` is the metadata address here too.
 */

/** The cloud metadata service, however it is spelled. */
const BLOCKED_HOSTS = new Set([
  '169.254.169.254',
  'fd00:ec2::254',
  'metadata.google.internal',
  'metadata',
])

export const BASE_URL_MESSAGES = {
  required: 'A base URL is required for this provider',
  scheme: 'The base URL must start with http:// or https://',
  parse: 'The base URL could not be parsed',
  credentials: 'The base URL must not carry a username or password',
  host: 'The base URL must name a host',
  metadata: 'That address is the cloud metadata endpoint and cannot be used',
} as const

/** The scheme as the server finds it: letters, digits, `+ - .` before `:`. */
function schemeOf(url: string): string {
  const colon = url.indexOf(':')
  if (colon < 1) return ''
  const head = url.slice(0, colon)
  return /^[A-Za-z][A-Za-z0-9+.-]*$/.test(head) ? head.toLowerCase() : ''
}

/** The authority: after `//`, up to the path, query or fragment. */
function authorityOf(url: string): string {
  const start = url.indexOf('//')
  if (start < 0) return ''
  const rest = url.slice(start + 2)
  const end = rest.search(/[/?#]/)
  return end < 0 ? rest : rest.slice(0, end)
}

/** `::ffff:a9fe:a9fe` as the IPv4 address it maps to. */
function unwrapMapped(host: string): string {
  const match = /^::ffff:([0-9a-f]{1,4}):([0-9a-f]{1,4})$/.exec(host)
  if (!match) return host
  const high = Number.parseInt(match[1], 16)
  const low = Number.parseInt(match[2], 16)
  return [high >> 8, high & 255, low >> 8, low & 255].join('.')
}

/**
 * The first problem with a typed endpoint, or null when it may be saved.
 *
 * @param value - What the trader typed.
 * @returns The sentence to show, or null.
 */
export function baseUrlProblem(value: string): string | null {
  const url = value.trim().replace(/\/+$/, '')
  if (!url) return BASE_URL_MESSAGES.required
  const scheme = schemeOf(url)
  if (scheme !== 'http' && scheme !== 'https') return BASE_URL_MESSAGES.scheme
  if (authorityOf(url).includes('@')) return BASE_URL_MESSAGES.credentials
  let parsed: URL
  try {
    parsed = new URL(url)
  } catch {
    return BASE_URL_MESSAGES.parse
  }
  const host = unwrapMapped(
    parsed.hostname
      .replace(/^\[|\]$/g, '')
      .trim()
      .toLowerCase()
      .replace(/\.+$/, '')
  )
  if (!host) return BASE_URL_MESSAGES.host
  if (BLOCKED_HOSTS.has(host)) return BASE_URL_MESSAGES.metadata
  return null
}
