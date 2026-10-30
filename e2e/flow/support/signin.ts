/**
 * Signing the trader in, for each journey and again whenever a session has
 * been signed out (decisions.md 28).
 *
 * The journeys run on the real clock. A login from before the daily
 * boundary (03:00 IST, the web's SESSION_EXPIRY_TIME) is answered on every
 * route as no login at all (web e2bdd061e), and the expiry poll then ends
 * the broker session too. So a single sign-in in the global setup would
 * fail every journey after 03:00 IST (21:30 UTC) in a run that spans it.
 * Instead:
 *
 * - each journey starts from a current sign-in (`SignIns.current`): the
 *   worker's sign-in is reused until a boundary has passed since it was
 *   made (`crossedBoundary`), then made again (`signIn`: the account
 *   login, the broker login when its session is gone, analyzer mode on).
 *   Reused, because the app limits logins per minute as the web does;
 * - no journey starts within GUARD_BEFORE_MS of the boundary: it waits
 *   until the boundary and the server's expiry poll after it have passed
 *   (`waitBeforeSignIn`), so the boundary never falls inside one;
 * - the journeys' own calls (support/http.ts `Session`) sign in again and
 *   retry once when an answer is 401 (`SignIns.renew`), and the journeys
 *   after them start from that sign-in.
 */

import { type APIRequestContext, request } from '@playwright/test'
import { CSRF_HEADER, csrfToken, SLOT, waitFor } from './http'

/** The daily boundary in IST (the app's default session expiry, 03:00). */
export const BOUNDARY_IST_MINUTES = 3 * 60
/** India has one offset and no daylight saving: UTC+05:30. */
const IST_OFFSET_MS = 330 * 60_000
const DAY_MS = 24 * 60 * 60_000
/** Longer than any journey takes, with room: none starts this close to the boundary. */
export const GUARD_BEFORE_MS = 180_000
/** The server's expiry poll runs every 30 s; after it has, a new login stays. */
export const SETTLE_AFTER_MS = 45_000

/** Milliseconds from `now` (epoch ms) to the next 03:00 IST (0 at it exactly). */
export function msUntilBoundary(now: number): number {
  const sinceIstMidnight = (((now + IST_OFFSET_MS) % DAY_MS) + DAY_MS) % DAY_MS
  const boundary = BOUNDARY_IST_MINUTES * 60_000
  return (boundary - sinceIstMidnight + DAY_MS) % DAY_MS
}

/**
 * How long to wait before signing in at `now` so the journey that follows
 * cannot span the boundary: none, unless `now` is within `before` of it or
 * less than `after` past it; then until `after` past it.
 */
export function waitBeforeSignIn(
  now: number,
  before = GUARD_BEFORE_MS,
  after = SETTLE_AFTER_MS
): number {
  const ahead = msUntilBoundary(now)
  if (ahead <= before) return ahead + after
  const past = DAY_MS - ahead
  if (past < after) return after - past
  return 0
}

/** Whether a 03:00 IST boundary fell after `signedInAt` and at or before `now`. */
export function crossedBoundary(signedInAt: number, now: number): boolean {
  return now > signedInAt && msUntilBoundary(signedInAt) <= now - signedInAt
}

/** Waits out the boundary when it is near (see `waitBeforeSignIn`). */
export async function clearOfBoundary(): Promise<void> {
  const wait = waitBeforeSignIn(Date.now())
  if (wait > 0) {
    await waitFor(
      'the 03:00 IST boundary to pass',
      async () => waitBeforeSignIn(Date.now()) === 0,
      wait + 30_000
    )
  }
}

export interface Trader {
  username: string
  password: string
}

/** The trader the global setup created, for the workers. */
export function traderFromSetup(): Trader {
  const raw = process.env.OPENALGO_E2E_TRADER
  if (!raw) throw new Error('OPENALGO_E2E_TRADER is not set: run with playwright.flow.config.ts')
  return JSON.parse(raw) as Trader
}

async function send(ctx: APIRequestContext, method: 'post' | 'put', url: string, data: unknown) {
  const token = await csrfToken(ctx)
  const r = await ctx[method](url, {
    data,
    headers: { [CSRF_HEADER]: token, Accept: 'application/json' },
  })
  const body = await r.json().catch(() => ({}))
  if (r.status() !== 200) throw new Error(`${url} answered ${r.status()}: ${JSON.stringify(body)}`)
  return body
}

async function getJson(ctx: APIRequestContext, url: string): Promise<any> {
  const r = await ctx.get(url, { headers: { Accept: 'application/json' } })
  return r.json().catch(() => ({}))
}

/** The account's login form, as the sign-in page posts it. */
export async function postForm(
  ctx: APIRequestContext,
  url: string,
  fields: Record<string, string>
): Promise<void> {
  const token = await csrfToken(ctx)
  const r = await ctx.post(url, {
    form: { ...fields, csrf_token: token },
    headers: { [CSRF_HEADER]: token, Accept: 'application/json' },
  })
  const body = await r.json().catch(() => ({}))
  if (r.status() !== 200 || body.status !== 'success') {
    throw new Error(`${url} answered ${r.status()}: ${JSON.stringify(body)}`)
  }
}

export type SignedIn = Awaited<ReturnType<APIRequestContext['storageState']>>

/**
 * A fresh sign-in: the account login, then the scripted broker's login if
 * its session is gone (a boundary or a sign-out ends it), and analyzer mode
 * on. Returns the cookies a browser context or a request context takes.
 */
export async function signIn(base: string, trader: Trader): Promise<SignedIn> {
  const ctx = await request.newContext({ baseURL: base })
  try {
    await postForm(ctx, '/auth/login', { username: trader.username, password: trader.password })
    const status = await getJson(ctx, '/auth/session-status')
    if (!(status.logged_in && status.broker)) {
      // Broker Configuration keeps the slot's keys; Select Broker logs in.
      await send(ctx, 'put', `/api/broker/credentials/${SLOT}`, {
        api_key: '1000000001',
        api_secret: 'scripted',
      })
      const login = await send(ctx, 'post', `/${SLOT}/callback`, {})
      if (login.status !== 'success') throw new Error(`Broker login: ${JSON.stringify(login)}`)
    }
    await waitFor('the scripted master contract', async () => {
      return (await getJson(ctx, '/api/master-contract/ready')).ready === true
    })
    // The navigation bar's Live/Analyze switch, only when it is off.
    if ((await getJson(ctx, '/auth/analyzer-mode'))?.data?.analyze_mode !== true) {
      const toggled = await send(ctx, 'post', '/auth/analyzer-toggle', {})
      if (toggled?.data?.analyze_mode !== true) {
        throw new Error(`Analyzer mode did not turn on: ${JSON.stringify(toggled)}`)
      }
    }
    return await ctx.storageState()
  } finally {
    await ctx.dispose()
  }
}

/**
 * The worker's sign-in, shared by its journeys: made on first use, made
 * again once a boundary has passed since (`current`) or when a call met a
 * signed-out session (`renew`).
 */
export class SignIns {
  state: SignedIn | undefined
  at = 0
  made = 0

  constructor(
    private readonly base: string,
    private readonly trader: Trader,
    private readonly now: () => number = Date.now,
    private readonly sign: (base: string, trader: Trader) => Promise<SignedIn> = signIn
  ) {}

  async current(): Promise<SignedIn> {
    await clearOfBoundary()
    if (!this.state || crossedBoundary(this.at, this.now())) await this.renew()
    return this.state!
  }

  async renew(): Promise<SignedIn> {
    await clearOfBoundary()
    this.state = await this.sign(this.base, this.trader)
    this.at = this.now()
    this.made += 1
    return this.state
  }
}
