/**
 * HTTP helpers shared by the global setup and the journeys: the session's
 * CSRF token, the signed-in trader's JSON calls (as the app's own pages make
 * them), and a bounded wait on an observable condition.
 */

import type { APIRequestContext } from '@playwright/test'

/** The catalogue slot the scripted broker takes (src-tauri/src/brokers/scripted.rs). */
export const SLOT = 'dhan_sandbox'
/** NIFTY's lot size in the scripted market. */
export const LOT = 65
/** The scripted market's NIFTY index level. */
export const SPOT = 25010

export const CSRF_HEADER = 'X-CSRFToken'

export async function csrfToken(ctx: APIRequestContext): Promise<string> {
  const r = await ctx.get('/auth/csrf-token', { headers: { Accept: 'application/json' } })
  const body = await r.json()
  if (!body?.csrf_token) throw new Error(`No CSRF token: ${r.status()}`)
  return body.csrf_token as string
}

/**
 * Polls `check` until it holds. Every wait in these journeys is on something
 * the app shows or answers, never a fixed sleep; the poll interval only
 * bounds how often it is asked.
 */
export async function waitFor(
  what: string,
  check: () => Promise<boolean>,
  timeoutMs = 60_000
): Promise<void> {
  const end = Date.now() + timeoutMs
  let delay = 50
  while (Date.now() < end) {
    if (await check()) return
    await new Promise((r) => setTimeout(r, delay))
    delay = Math.min(delay * 2, 500)
  }
  throw new Error(`Timed out after ${timeoutMs} ms waiting for ${what}`)
}

export interface Answer {
  status: number
  body: any
}

/**
 * The signed-in trader's JSON calls, with the session's CSRF token. A 401
 * (a session signed out: the 03:00 IST boundary, a sign-out) signs in again
 * through `reauth` and the call is made once more (decisions.md 28);
 * `reauths` counts how often that happened.
 */
export class Session {
  reauths = 0

  constructor(
    private readonly ctx: APIRequestContext,
    private readonly reauth: () => Promise<void> = async () => {
      throw new Error('This session cannot sign in again')
    }
  ) {}

  /** `call`, once more after signing in again if it answered 401. */
  private async signedIn(call: () => Promise<Answer>): Promise<Answer> {
    const first = await call()
    if (first.status !== 401) return first
    this.reauths += 1
    await this.reauth()
    return call()
  }

  private send(method: string, url: string, data?: unknown): Promise<Answer> {
    return this.signedIn(async () => {
      const headers: Record<string, string> = { Accept: 'application/json' }
      if (method !== 'GET') headers[CSRF_HEADER] = await csrfToken(this.ctx)
      const r = await this.ctx.fetch(url, { method, headers, data })
      const text = await r.text()
      let body: any = text
      try {
        body = text ? JSON.parse(text) : null
      } catch {
        // Not JSON: kept as text for the assertion that reads it.
      }
      return { status: r.status(), body }
    })
  }

  get(url: string) {
    return this.send('GET', url)
  }
  post(url: string, data?: unknown) {
    return this.send('POST', url, data ?? {})
  }
  put(url: string, data: unknown) {
    return this.send('PUT', url, data)
  }
  delete(url: string) {
    return this.send('DELETE', url)
  }

  /**
   * A raw body with its own content type (the 400 journeys), sent byte for
   * byte: Playwright JSON-encodes a string `data` under a JSON content type,
   * so `BUY NIFTY` reached the server as the JSON string "BUY NIFTY".
   */
  postRaw(url: string, body: string, contentType: string): Promise<Answer> {
    return this.signedIn(async () => {
      const r = await this.ctx.post(url, {
        headers: {
          Accept: 'application/json',
          'Content-Type': contentType,
          [CSRF_HEADER]: await csrfToken(this.ctx),
        },
        data: Buffer.from(body, 'utf8'),
      })
      return { status: r.status(), body: await r.json().catch(() => null) }
    })
  }

  /** The account's API key, as the API Key page reads it. */
  async apiKey(): Promise<string> {
    const { status, body } = await this.get('/apikey')
    if (status !== 200 || !body?.api_key) throw new Error(`No API key: ${status}`)
    return body.api_key as string
  }

  /** The scripted market's NIFTY future, as /api/v1/search lists it. */
  async niftyFuture(): Promise<string> {
    const apikey = await this.apiKey()
    const r = await this.ctx.post('/api/v1/search', {
      data: { apikey, query: 'NIFTY', exchange: 'NFO' },
    })
    const body = await r.json()
    const fut = (body?.data ?? []).find(
      (s: { symbol: string; instrumenttype?: string }) =>
        s.instrumenttype === 'FUT' || /FUT$/.test(s.symbol)
    )
    if (!fut) throw new Error(`No NIFTY future in the scripted market: ${JSON.stringify(body)}`)
    return fut.symbol as string
  }

  async workflow(id: number): Promise<any> {
    const { status, body } = await this.get(`/flow/api/workflows/${id}`)
    if (status !== 200) throw new Error(`Workflow ${id}: ${status} ${JSON.stringify(body)}`)
    return body
  }

  async workflows(): Promise<any[]> {
    const { body } = await this.get('/flow/api/workflows')
    return Array.isArray(body) ? body : []
  }

  async executions(id: number): Promise<any[]> {
    const { status, body } = await this.get(`/flow/api/workflows/${id}/executions?limit=200`)
    if (status !== 200) throw new Error(`Executions of ${id}: ${status} ${JSON.stringify(body)}`)
    return Array.isArray(body) ? body : []
  }

  /** The analyze-mode order book, as an API client reads it. */
  async orderBook(): Promise<any> {
    const apikey = await this.apiKey()
    const r = await this.ctx.post('/api/v1/orderbook', { data: { apikey } })
    return r.json()
  }
}
