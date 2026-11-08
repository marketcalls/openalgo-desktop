/**
 * Desktop-only: broker keys, one set per broker, for as many brokers as the
 * trader likes, with exactly one broker logged in at a time.
 *
 * OpenAlgo web keeps a single broker's keys in `.env`. The desktop has no
 * `.env`, so the keys are saved in the app, per broker (an approved
 * deviation from the web's UI, modelled on OpenBull's Broker Configuration
 * and Select Broker pages; see CLAUDE.md, "The compatibility contract").
 *
 * The server never returns a saved key or secret: `saved` is a masked
 * display ("abc***xyz") and the page offers a separate, empty replacement
 * input. A field left blank keeps what is saved.
 */

import type { BrokerSignIn } from '@/lib/desktop'

/** The keys a broker can need, by the server's field names. */
export type BrokerKeyName =
  | 'api_key'
  | 'api_secret'
  | 'client_id'
  | 'api_key_market'
  | 'api_secret_market'

/** One key a broker needs, labelled the way that broker names it. */
export interface BrokerKeyField {
  key: BrokerKeyName
  label: string
  hint: string
  /** The replacement input hides what is typed. */
  secret: boolean
  /** Needed before the first sign-in. */
  required: boolean
  /** Masked display of the saved value, or null when nothing is saved. */
  saved: string | null
}

/**
 * An API plan a broker sells with different request limits (Fyers Standard
 * or Prime). OpenAlgo paces the broker's requests for the chosen one.
 */
export interface RatePlanOption {
  id: string
  label: string
  /** The limits the plan allows, for the trader. */
  hint: string
}

/** One broker as the Select Broker and Broker Configuration pages show it. */
export interface BrokerEntry {
  id: string
  name: string
  /** Keys are saved for this broker. */
  configured: boolean
  /** This broker is logged in now (at most one broker is). */
  active: boolean
  sign_in: BrokerSignIn
  exchanges: string[]
  /** How this broker's sign-in works, in a sentence or two. */
  banner: string
  fields: BrokerKeyField[]
  /** The redirect URL in use (redirect brokers only). */
  redirect_url: string | null
  /** The automatic redirect URL: this app's address, or the Host Server URL. */
  redirect_url_automatic: string | null
  /** True while the redirect URL is the automatic one. */
  redirect_url_is_default: boolean
  /** What to do with the redirect URL at the broker (redirect brokers only). */
  redirect_hint: string | null
  /** The broker's API plans, the default first; empty for most brokers. */
  rate_plans?: RatePlanOption[]
  /** The plan requests are paced for now (brokers with plans only). */
  rate_plan?: string | null
}

export interface BrokerList {
  /** The broker logged in now, or null. */
  logged_in: string | null
  /**
   * Why the logged-in broker's session cannot end for a switch now (Live
   * strategies, OpenScript runs or scalping protection still running,
   * orders not yet settled), or null. The server refuses such a switch
   * whatever the page does.
   */
  switch_refusal?: string | null
  brokers: BrokerEntry[]
}

/** What a save sends: blank or missing keys keep the saved value. */
export type BrokerKeyUpdate = Partial<Record<BrokerKeyName, string>> & {
  /** Sent only when changed; an empty string means the automatic URL. */
  redirect_url?: string
}

export interface SaveOutcome {
  message: string
  /** The saved keys were the logged-in broker's, whose session then ended. */
  signed_out: boolean
}

/** Thrown with a message written for the trader. */
export class BrokerConfigError extends Error {
  readonly status: number
  constructor(message: string, status: number) {
    super(message)
    this.status = status
  }
}

const UNREACHABLE = 'Could not reach OpenAlgo. Try again.'

async function csrfToken(): Promise<string> {
  const res = await fetch('/auth/csrf-token', { credentials: 'include' }).catch(() => null)
  const body = res?.ok ? await res.json().catch(() => null) : null
  const token = body?.csrf_token
  if (typeof token !== 'string' || !token) throw new BrokerConfigError(UNREACHABLE, 0)
  return token
}

async function request<T>(url: string, init?: RequestInit): Promise<T> {
  let res: Response
  try {
    res = await fetch(url, { credentials: 'include', ...init })
  } catch {
    throw new BrokerConfigError(UNREACHABLE, 0)
  }
  const body = await res.json().catch(() => null)
  if (!res.ok || body?.status === 'error') {
    const message =
      typeof body?.message === 'string' && body.message
        ? body.message
        : 'That did not work. Try again.'
    throw new BrokerConfigError(message, res.status)
  }
  return body as T
}

async function write<T>(url: string, method: 'PUT' | 'POST' | 'DELETE', payload?: unknown) {
  const csrf = await csrfToken()
  return request<T>(url, {
    method,
    headers: { 'Content-Type': 'application/json', 'X-CSRFToken': csrf },
    body: payload === undefined ? undefined : JSON.stringify(payload),
  })
}

const brokerPath = (id: string) => `/api/broker/credentials/${encodeURIComponent(id)}`

/** Why a broker sign-in that came back to Select Broker failed. */
export interface SignInFailure {
  broker: string
  message: string
}

export const brokerConfigApi = {
  /** Every broker, with what is saved (masked) and which one is logged in. */
  async list(): Promise<BrokerList> {
    const body = await request<{ data: BrokerList }>('/api/broker/list')
    return body.data
  },

  /**
   * Desktop: the reason a failed sign-in sent this browser back with
   * (`/broker?error=<id>`). The address carries only an id; the server
   * keeps the sentence for this session and answers it once. Null when it
   * is unknown, already read, expired or another session's.
   */
  async signInFailure(id: string): Promise<SignInFailure | null> {
    try {
      const body = await request<{ data?: Partial<SignInFailure> }>(
        `/api/broker/sign-in-failure/${encodeURIComponent(id)}`
      )
      const broker = body.data?.broker
      const message = body.data?.message
      return typeof broker === 'string' && typeof message === 'string' && message
        ? { broker, message }
        : null
    } catch {
      return null
    }
  },

  /**
   * Save a broker's keys. Blank keys keep the saved value. New keys for the
   * broker that is logged in end its session, and only with `signOut`
   * (the page confirms first); without it the server saves nothing.
   */
  async save(id: string, update: BrokerKeyUpdate, signOut = false): Promise<SaveOutcome> {
    const body = await write<{ message?: string; signed_out?: boolean }>(
      `${brokerPath(id)}${signOut ? '?sign_out=true' : ''}`,
      'PUT',
      update
    )
    return { message: body.message ?? 'Saved.', signed_out: body.signed_out === true }
  },

  /**
   * Pace the broker's requests for one of its API plans. Applies at once;
   * the session is not touched. Returns the server's sentence.
   */
  async setRatePlan(id: string, plan: string): Promise<string> {
    const body = await write<{ message?: string }>(
      `/api/broker/rate-plan/${encodeURIComponent(id)}`,
      'PUT',
      { plan }
    )
    return body.message ?? 'Saved.'
  },

  /**
   * Remove a broker's saved keys. Forgetting the broker that is logged in
   * signs out of it first, and only with `signOut` (the page confirms).
   */
  async forget(id: string, signOut: boolean): Promise<string> {
    const body = await write<{ message?: string }>(
      `${brokerPath(id)}${signOut ? '?sign_out=true' : ''}`,
      'DELETE'
    )
    return body.message ?? 'Forgotten.'
  },
}

/** Brokers in the order the pages list them: logged in, configured, the rest. */
export function sortBrokers(brokers: readonly BrokerEntry[]): BrokerEntry[] {
  const rank = (b: BrokerEntry) => (b.active ? 0 : b.configured ? 1 : 2)
  return [...brokers].sort((a, b) => rank(a) - rank(b) || a.name.localeCompare(b.name))
}

/** The status a broker's badge shows. */
export function brokerStatus(b: Pick<BrokerEntry, 'active' | 'configured'>) {
  if (b.active) return 'Active' as const
  return b.configured ? ('Configured' as const) : ('Not Configured' as const)
}
