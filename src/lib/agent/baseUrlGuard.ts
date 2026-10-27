/**
 * The one guard on a provider endpoint the trader types (D-028).
 *
 * The add dialog and the edit dialog both run it, and the server runs the
 * same steps (`agent::providers::base_url`) on the same cases
 * (`src-tauri/tests/fixtures/agent/base_url_cases.json`), so the three cannot
 * disagree. On the web they did: the add dialog missed an IPv4-mapped IPv6
 * spelling of the metadata address and the edit dialog checked nothing.
 *
 * OpenAlgo makes requests to this address, so it is checked before it is
 * saved: http or https only, no username or password in it, a host, and never
 * the cloud metadata service. A server on the trader's own computer or network
 * is allowed on purpose: that is the point of the two kinds that take an
 * endpoint.
 *
 * Every decision is made on one parse, the URL standard's, which is how the
 * request reads the address too. Text a parser may read two ways is refused
 * first: a backslash (read as a slash) and any space, tab, line break or
 * control character (dropped by the standard). The host decides by what it
 * is, however it is spelled (`2130706433` and `[::ffff:127.0.0.1]` are this
 * computer; `2852039166` is the metadata address).
 */

export type HostClass = 'metadata' | 'loopback' | 'other'

const METADATA_HOSTS = new Set([
  '169.254.169.254',
  'fd00:ec2::254',
  'metadata.google.internal',
  'metadata',
])

export const BASE_URL_MESSAGES = {
  required: 'A base URL is required for this provider',
  characters:
    'The base URL must not contain spaces, line breaks, control characters or backslashes',
  scheme: 'The base URL must start with http:// or https://',
  parse: 'The base URL could not be parsed',
  credentials: 'The base URL must not carry a username or password',
  host: 'The base URL must name a host',
  metadata: 'That address is the cloud metadata endpoint and cannot be used',
} as const

/** A backslash, whitespace or a control character anywhere. */
// biome-ignore lint/suspicious/noControlCharactersInRegex: control characters are what it refuses
const AMBIGUOUS = /[\\\s\u0000-\u001f\u007f-\u009f]/

/** The scheme as written (lower-cased), or empty. */
function schemeOf(url: string): string {
  const colon = url.indexOf(':')
  if (colon < 1) return ''
  const head = url.slice(0, colon)
  return /^[A-Za-z][A-Za-z0-9+.-]*$/.test(head) ? head.toLowerCase() : ''
}

/** `::ffff:7f00:1` as the IPv4 address it maps to. */
function unwrapMapped(host: string): string {
  const match = /^::ffff:([0-9a-f]{1,4}):([0-9a-f]{1,4})$/.exec(host)
  if (!match) return host
  const high = Number.parseInt(match[1], 16)
  const low = Number.parseInt(match[2], 16)
  return [high >> 8, high & 255, low >> 8, low & 255].join('.')
}

/** What a parsed URL's host is; null when it names none. */
export function hostClass(parsed: URL): HostClass | null {
  const host = unwrapMapped(
    parsed.hostname
      .replace(/^\[|\]$/g, '')
      .toLowerCase()
      .replace(/\.+$/, '')
  )
  if (!host) return null
  if (METADATA_HOSTS.has(host)) return 'metadata'
  if (
    host === 'localhost' ||
    host.endsWith('.localhost') ||
    host === '::1' ||
    /^127\.\d+\.\d+\.\d+$/.test(host)
  ) {
    return 'loopback'
  }
  return 'other'
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
  if (AMBIGUOUS.test(url)) return BASE_URL_MESSAGES.characters
  const scheme = schemeOf(url)
  if (scheme !== 'http' && scheme !== 'https') return BASE_URL_MESSAGES.scheme
  let parsed: URL
  try {
    parsed = new URL(url)
  } catch {
    return BASE_URL_MESSAGES.parse
  }
  if (parsed.protocol !== 'http:' && parsed.protocol !== 'https:') return BASE_URL_MESSAGES.scheme
  if (parsed.username || parsed.password) return BASE_URL_MESSAGES.credentials
  const cls = hostClass(parsed)
  if (cls === null) return BASE_URL_MESSAGES.host
  if (cls === 'metadata') return BASE_URL_MESSAGES.metadata
  return null
}
