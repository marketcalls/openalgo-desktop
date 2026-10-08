/**
 * Desktop: the runner page's channel to the app. Every call names the run and
 * carries the run's own secret, which the app put in this page's URL fragment
 * (a fragment never reaches a server or a log). A 403 means the app no longer
 * knows the run, and the page stops.
 */

import type { HostBar, OrderIntent } from 'openalgo-script'
import { type InboxMessage, RunGone, type RunSpec, type Transport } from './driver'

/** The run and its secret, from `#run=<id>&token=<secret>`. */
export function readFragment(hash: string): { run: string; token: string } | null {
  const params = new URLSearchParams(hash.replace(/^#/, ''))
  const run = params.get('run')
  const token = params.get('token')
  if (!run || !token) return null
  return { run, token }
}

export function httpTransport(
  run: string,
  token: string,
  fetcher: typeof fetch = fetch
): Transport {
  const base = `/openscript/runner/host/${encodeURIComponent(run)}`
  const call = async (path: string, init: RequestInit = {}): Promise<unknown> => {
    const response = await fetcher(`${base}${path}`, {
      ...init,
      headers: {
        Accept: 'application/json',
        'X-Runner-Token': token,
        ...(init.body ? { 'Content-Type': 'application/json' } : {}),
      },
      credentials: 'omit',
      cache: 'no-store',
    })
    if (response.status === 403) throw new RunGone()
    const body = (await response.json()) as { status?: string; message?: string }
    if (!response.ok || body?.status !== 'success') {
      throw new Error(body?.message || 'The app did not answer.')
    }
    return body
  }
  const post = (path: string, payload: unknown) =>
    call(path, { method: 'POST', body: JSON.stringify(payload) })

  return {
    async spec() {
      return (await call('/spec')) as RunSpec
    },
    async bars() {
      const body = (await call('/bars')) as { data?: HostBar[] }
      return body.data ?? []
    },
    async inbox(after: number) {
      const body = (await call(`/inbox?after=${after}`)) as {
        messages?: InboxMessage[]
        stop?: boolean
      }
      return { messages: body.messages ?? [], stop: body.stop === true }
    },
    async intents(intents: readonly OrderIntent[]) {
      await post('/intents', { confirmed: true, intents })
    },
    async log(lines: readonly string[]) {
      await post('/log', { lines })
    },
    async ended(message: string) {
      await post('/ended', { message })
    },
  }
}
