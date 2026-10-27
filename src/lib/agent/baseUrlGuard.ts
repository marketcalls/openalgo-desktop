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
 * first: a backslash (read as a slash), any space, tab, line break or control
 * character (dropped by the standard), and any '@' (where a user name starts).
 * The host decides by what it is, however it is spelled (`2130706433` and
 * `[::ffff:127.0.0.1]` are this computer; `2852039166`, `[::a9fe:a9fe]` and
 * `[64:ff9b::a9fe:a9fe]` are the metadata address). The whole link-local
 * blocks are refused, as the server's HTTP client refuses them.
 */

export type HostClass = 'metadata' | 'loopback' | 'other'

/** The cloud metadata service's names (its addresses are classed below). */
const METADATA_NAMES = new Set(['metadata.google.internal', 'metadata'])

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

/** An IPv6 literal (as the URL standard writes it: hex, `::`) as 8 numbers. */
function ipv6Segments(host: string): number[] | null {
  if (!host.includes(':')) return null
  const halves = host.split('::')
  if (halves.length > 2) return null
  const split = (s: string) => (s ? s.split(':') : [])
  const head = split(halves[0])
  const tail = halves.length === 2 ? split(halves[1]) : []
  const fill = halves.length === 2 ? 8 - head.length - tail.length : 0
  const all = [...head, ...Array<string>(Math.max(fill, 0)).fill('0'), ...tail]
  if (all.length !== 8 || !all.every((x) => /^[0-9a-f]{1,4}$/.test(x))) return null
  return all.map((x) => Number.parseInt(x, 16))
}

/**
 * The IPv4 address an IPv6 address carries: IPv4-mapped (`::ffff:a.b.c.d`),
 * IPv4-compatible (`::a.b.c.d`, not `::` or `::1`) or NAT64
 * (`64:ff9b::a.b.c.d`); null for any other.
 */
function carriedV4(s: number[]): number[] | null {
  const zero = (from: number, to: number) => s.slice(from, to).every((x) => x === 0)
  const mapped = zero(0, 5) && s[5] === 0xffff
  const compatible = zero(0, 6) && !(s[6] === 0 && s[7] <= 1)
  const nat64 = s[0] === 0x64 && s[1] === 0xff9b && zero(2, 6)
  if (!mapped && !compatible && !nat64) return null
  return [s[6] >> 8, s[6] & 255, s[7] >> 8, s[7] & 255]
}

/** An IPv4 address's class: the link-local block, this computer, other. */
function v4Class(octets: number[]): HostClass {
  if (octets[0] === 169 && octets[1] === 254) return 'metadata'
  if (octets[0] === 127) return 'loopback'
  return 'other'
}

/**
 * What a parsed URL's host is; null when it names none. Metadata covers the
 * service's names and the link-local blocks it lives in (169.254.0.0/16,
 * fe80::/10, fd00:ec2::/32), the rule the server's HTTP client connects by.
 */
export function hostClass(parsed: URL): HostClass | null {
  const host = parsed.hostname
    .replace(/^\[|\]$/g, '')
    .toLowerCase()
    .replace(/\.+$/, '')
  if (!host) return null
  if (METADATA_NAMES.has(host)) return 'metadata'
  if (host === 'localhost' || host.endsWith('.localhost')) return 'loopback'
  const v4 = /^(\d+)\.(\d+)\.(\d+)\.(\d+)$/.exec(host)
  if (v4) return v4Class(v4.slice(1).map(Number))
  const v6 = ipv6Segments(host)
  if (!v6) return 'other'
  const carried = carriedV4(v6)
  if (carried) return v4Class(carried)
  if ((v6[0] & 0xffc0) === 0xfe80 || (v6[0] === 0xfd00 && v6[1] === 0x0ec2)) return 'metadata'
  if (v6.slice(0, 7).every((x) => x === 0) && v6[7] === 1) return 'loopback'
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
  // Any '@' at all, as the web refuses one: the same in every parser, and no
  // provider endpoint needs one. The parsed user name is checked again below.
  if (url.includes('@')) return BASE_URL_MESSAGES.credentials
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
