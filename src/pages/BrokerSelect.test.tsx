import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { fireEvent, render, screen, waitFor } from '@/test/test-utils'
import BrokerSelect from './BrokerSelect'

// Desktop: the broker page starts each sign-in the way the server reports it
// (`sign_in`), so it cannot drift from the server's broker catalogue.

const realLocation = window.location
let assigned: string[] = []
let posts: string[] = []

function serve(broker: string, signIn: string) {
  vi.stubGlobal(
    'fetch',
    vi.fn(async (url: string, init?: RequestInit) => {
      const json = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status })
      if (url === '/auth/broker-config') {
        return json({
          status: 'success',
          broker_name: broker,
          broker_api_key: null,
          redirect_url: `http://127.0.0.1:5000/${broker}/callback`,
          sign_in: signIn,
        })
      }
      if (url === '/api/broker/configured') {
        return json({
          status: 'success',
          data: { brokers: [{ name: broker, active: true, sign_in: signIn }], active: broker },
        })
      }
      if (url === '/auth/csrf-token') return json({ csrf_token: 'tok' })
      if (init?.method === 'POST') {
        posts.push(url)
        return json({ status: 'success', redirect: '/dashboard' })
      }
      return json({}, 404)
    })
  )
}

beforeEach(() => {
  assigned = []
  posts = []
  const loc = { ...realLocation, origin: 'http://127.0.0.1:5500', search: '' }
  Object.defineProperty(loc, 'href', {
    get: () => assigned[assigned.length - 1] ?? 'http://127.0.0.1:5500/broker',
    set: (v: string) => assigned.push(v),
  })
  Object.defineProperty(window, 'location', { configurable: true, value: loc })
})

afterEach(() => {
  Object.defineProperty(window, 'location', { configurable: true, value: realLocation })
  vi.unstubAllGlobals()
})

async function connect() {
  const button = await screen.findByRole('button', { name: /connect account/i })
  await waitFor(() => expect(button).toBeEnabled())
  fireEvent.click(button)
}

describe('BrokerSelect sign-in start', () => {
  it.each(['shoonya', 'zebu', 'tradesmart', 'rmoney', 'zerodha', 'flattrade'])(
    'sends %s through the server-side sign-in start',
    async (broker) => {
      serve(broker, 'redirect')
      render(<BrokerSelect />)
      await connect()
      await waitFor(() => expect(assigned).toEqual([`/${broker}/initiate-oauth`]))
    }
  )

  it.each([
    'fivepaisaxts',
    'jainamxts',
    'ibulls',
    'iifl',
    'wisdom',
    'deltaexchange',
    'dhan_sandbox',
  ])('signs %s in from the saved keys with one action, without a form', async (broker) => {
    serve(broker, 'saved_keys')
    render(<BrokerSelect />)
    await connect()
    await waitFor(() => expect(assigned).toEqual(['/dashboard']))
    expect(posts).toEqual([`/${broker}/callback`])
  })

  it("shows the server's reason when a saved-keys sign-in is refused", async () => {
    serve('fivepaisaxts', 'saved_keys')
    const ok = globalThis.fetch as unknown as (u: string, i?: RequestInit) => Promise<Response>
    vi.stubGlobal(
      'fetch',
      vi.fn(async (url: string, init?: RequestInit) =>
        init?.method === 'POST'
          ? new Response(
              JSON.stringify({ status: 'error', message: 'Check your API key and secret.' }),
              { status: 401 }
            )
          : ok(url, init)
      )
    )
    render(<BrokerSelect />)
    await connect()
    expect(await screen.findByText('Check your API key and secret.')).toBeInTheDocument()
    expect(assigned).toEqual([])
    expect(screen.getByRole('button', { name: /connect account/i })).toBeEnabled()
  })

  it('opens the in-app form for a form broker', async () => {
    serve('angel', 'form')
    render(<BrokerSelect />)
    await connect()
    await waitFor(() => expect(assigned).toEqual(['/angel/callback']))
    expect(posts).toEqual([])
  })
})
