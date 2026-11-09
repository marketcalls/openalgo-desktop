import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { BrokerList } from '@/api/broker-config'
import { useAuthStore } from '@/stores/authStore'
import { useBrokerStore } from '@/stores/brokerStore'
import { brokerEntry, brokerList, saved } from '@/test/brokerList'
import { renderWithQuery } from '@/test/queryRender'
import { fireEvent, screen, waitFor, within } from '@/test/test-utils'
import BrokerSelect from './BrokerSelect'

// Desktop: Select Broker is a card per broker (OpenBull's pattern). Keys are
// saved for many brokers; one broker is logged in at a time. "Login with"
// starts each sign-in the way the server reports it (`sign_in`).

const realLocation = window.location
let assigned: string[] = []
let posts: string[] = []
let list: BrokerList

let listReads = 0
/** The sign-in failure ids the page asked OpenAlgo about. */
let failureReads: string[] = []

function serve(
  opts: {
    refuseSavedKeys?: string
    /** A saved-keys sign-in whose reply never arrives. */
    loseSignInReply?: boolean
    capabilities?: () => Promise<Response>
    /** Reasons OpenAlgo keeps for refused sign-ins, by id; each read once. */
    failures?: Record<string, { broker: string; message: string }>
  } = {}
) {
  const failures = { ...opts.failures }
  vi.stubGlobal(
    'fetch',
    vi.fn(async (url: string, init?: RequestInit) => {
      const json = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status })
      if (url === '/api/broker/list') {
        listReads += 1
        return json({ status: 'success', data: list })
      }
      if (url === '/api/broker/capabilities' && opts.capabilities) return opts.capabilities()
      if (url === '/auth/session-status') {
        return json({
          status: 'success',
          logged_in: list.logged_in !== null,
          broker: list.logged_in,
        })
      }
      if (url === '/auth/csrf-token') return json({ csrf_token: 'tok' })
      const failurePath = '/api/broker/sign-in-failure/'
      if (url.startsWith(failurePath)) {
        const id = decodeURIComponent(url.slice(failurePath.length))
        failureReads.push(id)
        const kept = Object.hasOwn(failures, id) ? failures[id] : undefined
        delete failures[id]
        return kept
          ? json({ status: 'success', data: kept })
          : json({ status: 'error', message: 'The broker login did not complete. Try again.' }, 404)
      }
      if (init?.method === 'POST') {
        posts.push(url)
        if (opts.loseSignInReply) throw new TypeError('Failed to fetch')
        if (opts.refuseSavedKeys) {
          return json({ status: 'error', message: opts.refuseSavedKeys }, 401)
        }
        return json({ status: 'success', redirect: '/dashboard' })
      }
      return json({}, 404)
    })
  )
}

function renderPage(path = '/broker') {
  // The router reads the address from the stand-in location.
  const url = new URL(path, 'http://127.0.0.1:5500')
  Object.assign(window.location, { pathname: url.pathname, search: url.search, hash: '' })
  return renderWithQuery(<BrokerSelect />)
}

const card = async (id: string) => within(await screen.findByTestId(`broker-card-${id}`))

beforeEach(() => {
  assigned = []
  posts = []
  listReads = 0
  failureReads = []
  useBrokerStore.getState().clearCapabilities()
  useAuthStore.setState({
    user: { username: 'trader', broker: 'zerodha', isLoggedIn: true, loginTime: null },
    isAuthenticated: true,
  })
  list = brokerList([
    brokerEntry('zerodha', 'Zerodha', 'redirect'),
    brokerEntry('angel', 'Angel One', 'form'),
    brokerEntry('fivepaisaxts', '5 Paisa (XTS)', 'saved_keys'),
    brokerEntry('fyers', 'Fyers', 'redirect'),
  ])
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

describe('Select Broker cards', () => {
  it('shows one badge per broker, configured brokers first, with their exchanges', async () => {
    list = brokerList([
      brokerEntry('zerodha', 'Zerodha', 'redirect'),
      saved(brokerEntry('fyers', 'Fyers', 'redirect'), { api_key: 'APP***100' }),
      { ...saved(brokerEntry('angel', 'Angel One', 'form'), {}), active: true },
    ])
    serve()
    renderPage()
    const cards = await screen.findAllByTestId(/^broker-card-/)
    expect(cards.map((c) => c.dataset.testid)).toEqual([
      'broker-card-angel',
      'broker-card-fyers',
      'broker-card-zerodha',
    ])
    expect((await card('angel')).getByText('Active')).toBeInTheDocument()
    expect((await card('fyers')).getByText('Configured')).toBeInTheDocument()
    expect((await card('zerodha')).getByText('Not Configured')).toBeInTheDocument()
    expect(screen.getAllByText('Active')).toHaveLength(1)
    expect((await card('zerodha')).getByText('Exchanges: NSE, BSE, NFO')).toBeInTheDocument()
  })

  it('offers Configure for a broker without keys, opening its configuration', async () => {
    serve()
    renderPage()
    const link = (await card('zerodha')).getByRole('link', { name: 'Configure' })
    expect(link).toHaveAttribute('href', '/broker/config?broker=zerodha')
    expect((await card('zerodha')).queryByRole('button', { name: /login with/i })).toBeNull()
  })

  // Web BrokerSelect.test.tsx (eec700046), the same case.
  it('offers Edit keys on a configured broker and Broker Configuration in the header', async () => {
    list = brokerList([saved(brokerEntry('fyers', 'Fyers', 'redirect'), {})])
    serve()
    renderPage()
    expect((await card('fyers')).getByRole('link', { name: 'Edit Fyers keys' })).toHaveAttribute(
      'href',
      '/broker/config?broker=fyers'
    )
    expect(screen.getByRole('link', { name: /Broker Configuration/ })).toHaveAttribute(
      'href',
      '/broker/config'
    )
  })

  it('filters the cards by name', async () => {
    serve()
    renderPage()
    await card('zerodha')
    fireEvent.change(screen.getByRole('textbox', { name: 'Search brokers' }), {
      target: { value: 'ang' },
    })
    expect(screen.getAllByTestId(/^broker-card-/).map((c) => c.dataset.testid)).toEqual([
      'broker-card-angel',
    ])
    fireEvent.change(screen.getByRole('textbox', { name: 'Search brokers' }), {
      target: { value: 'nothing' },
    })
    expect(screen.getByText('No broker matches "nothing".')).toBeInTheDocument()
  })

  // Web BrokerSelect.test.tsx (eec700046), the same case.
  it('shows the data subscription note on a configured Zerodha card only', async () => {
    list = brokerList([
      saved(brokerEntry('zerodha', 'Zerodha', 'redirect'), {}),
      brokerEntry('dhan', 'Dhan', 'redirect'),
    ])
    serve()
    renderPage()
    expect(
      (await card('zerodha')).getByText(/Zerodha requires an active Kite Connect/)
    ).toBeInTheDocument()
    expect((await card('dhan')).queryByText(/Dhan requires/)).toBeNull()
  })

  it('says the Dhan Sandbox prices are simulated on its card, configured or active', async () => {
    list = brokerList([
      saved(brokerEntry('dhan_sandbox', 'Dhan (Sandbox)', 'saved_keys'), {}),
      brokerEntry('dhan', 'Dhan', 'redirect'),
    ])
    serve()
    renderPage()
    expect(
      (await card('dhan_sandbox')).getByText(/Simulated prices, not market prices/)
    ).toBeInTheDocument()
    expect((await card('dhan')).queryByText(/Simulated prices/)).toBeNull()
  })
})

// A refused broker login comes back as /broker?error=<id>&broker=<id>.
// Desktop: the id names the reason OpenAlgo kept for this browser session,
// read once; the web maps a fixed code to a fixed sentence. Either way the
// address is built by whoever sent the link, so its text never reaches the
// page.
describe('A login that came back refused', () => {
  const REASON = 'The broker did not complete the sign-in.'

  it("shows the reason OpenAlgo kept on that broker's card", async () => {
    list = brokerList([saved(brokerEntry('zerodha', 'Zerodha', 'redirect'), {})])
    serve({ failures: { 'id-1': { broker: 'zerodha', message: REASON } } })
    renderPage('/broker?error=id-1&broker=zerodha')
    expect(await (await card('zerodha')).findByText(REASON)).toBeInTheDocument()
    expect(failureReads).toEqual(['id-1'])
  })

  // Web BrokerSelect.test.tsx (eec700046), the same case.
  it('never shows text from the address', async () => {
    list = brokerList([saved(brokerEntry('zerodha', 'Zerodha', 'redirect'), {})])
    serve()
    renderPage('/broker?error=%3Cb%3EINJECTED-TEXT%3C%2Fb%3E&broker=zerodha')
    expect(
      await (await card('zerodha')).findByText('The broker login did not complete. Try again.')
    ).toBeInTheDocument()
    expect(screen.queryByText(/INJECTED-TEXT/)).toBeNull()
  })

  it('puts the reason on the broker OpenAlgo names, not the one in the address', async () => {
    list = brokerList([
      saved(brokerEntry('zerodha', 'Zerodha', 'redirect'), {}),
      saved(brokerEntry('angel', 'Angel One', 'form'), {}),
    ])
    serve({ failures: { 'id-2': { broker: 'zerodha', message: REASON } } })
    renderPage('/broker?error=id-2&broker=angel')
    expect(await (await card('zerodha')).findByText(REASON)).toBeInTheDocument()
    expect((await card('angel')).queryByText(REASON)).toBeNull()
  })

  // Web BrokerSelect.test.tsx (eec700046) 'shows it at the top when no listed
  // broker is named': a reason for a broker without a card was never shown.
  it('shows it at the top when no listed broker is named', async () => {
    serve({ failures: { 'id-3': { broker: 'nosuchbroker', message: REASON } } })
    renderPage('/broker?error=id-3&broker=nosuchbroker')
    const message = await screen.findByText(REASON)
    expect(message.closest('[data-testid^="broker-card-"]')).toBeNull()
  })
})

describe('Login with a broker', () => {
  it.each([
    ['zerodha', 'Zerodha', 'redirect', '/zerodha/initiate-oauth'],
    ['shoonya', 'Shoonya', 'redirect', '/shoonya/initiate-oauth'],
    ['angel', 'Angel One', 'form', '/angel/callback'],
    ['kotak', 'Kotak Securities', 'form', '/kotak/callback'],
  ] as const)('starts %s the way the server says it signs in', async (id, name, signIn, to) => {
    list = brokerList([saved(brokerEntry(id, name, signIn), {})])
    serve()
    renderPage()
    fireEvent.click((await card(id)).getByRole('button', { name: `Login with ${name}` }))
    await waitFor(() => expect(assigned).toEqual([to]))
    expect(posts).toEqual([])
  })

  it('signs a saved-keys broker in with one action, without a form', async () => {
    list = brokerList([saved(brokerEntry('fivepaisaxts', '5 Paisa (XTS)', 'saved_keys'), {})])
    serve()
    renderPage()
    fireEvent.click(
      (await card('fivepaisaxts')).getByRole('button', { name: 'Login with 5 Paisa (XTS)' })
    )
    await waitFor(() => expect(assigned).toEqual(['/dashboard']))
    expect(posts).toEqual(['/fivepaisaxts/callback'])
  })

  it("shows the server's reason on the card when a saved-keys sign-in is refused", async () => {
    list = brokerList([saved(brokerEntry('fivepaisaxts', '5 Paisa (XTS)', 'saved_keys'), {})])
    serve({ refuseSavedKeys: 'Check your API key and secret.' })
    renderPage()
    fireEvent.click(
      (await card('fivepaisaxts')).getByRole('button', { name: 'Login with 5 Paisa (XTS)' })
    )
    expect(
      await (await card('fivepaisaxts')).findByText('Check your API key and secret.')
    ).toBeInTheDocument()
    expect(assigned).toEqual([])
  })
})

// One broker at a time: logging in to another ends the live session, so it
// is confirmed first, and the session ends before the new sign-in starts.
describe('Login while another broker is logged in', () => {
  beforeEach(() => {
    list = brokerList([
      { ...saved(brokerEntry('zerodha', 'Zerodha', 'redirect'), {}), active: true },
      saved(brokerEntry('fyers', 'Fyers', 'redirect'), {}),
    ])
  })

  it('asks first, and Cancel ends nothing and starts nothing', async () => {
    serve()
    renderPage()
    fireEvent.click((await card('fyers')).getByRole('button', { name: 'Login with Fyers' }))
    expect(await screen.findByText('End your Zerodha session?')).toBeInTheDocument()
    expect(
      screen.getByText(
        /Signing in to Fyers ends your Zerodha session. Open positions and orders stay with Zerodha./
      )
    ).toBeInTheDocument()
    fireEvent.click(screen.getByRole('button', { name: 'Cancel' }))
    await waitFor(() => expect(screen.queryByText('End your Zerodha session?')).toBeNull())
    expect(posts).toEqual([])
    expect(assigned).toEqual([])
  })

  it('starts the new sign-in without ending the live session', async () => {
    serve()
    renderPage()
    fireEvent.click((await card('fyers')).getByRole('button', { name: 'Login with Fyers' }))
    expect(await screen.findByText(/if it does not, Zerodha stays connected/)).toBeInTheDocument()
    fireEvent.click(await screen.findByRole('button', { name: 'Log in to Fyers' }))
    await waitFor(() => expect(assigned).toEqual(['/fyers/initiate-oauth']))
    // The server ends Zerodha's session only when Fyers' sign-in completes.
    expect(posts).toEqual([])
  })

  it('offers the dashboard on the logged-in broker instead of a second login', async () => {
    serve()
    renderPage()
    expect(
      (await card('zerodha')).getByRole('button', { name: 'Open Dashboard' })
    ).toBeInTheDocument()
    expect((await card('zerodha')).queryByRole('button', { name: /login with/i })).toBeNull()
  })
})

// FLOW-03 (client): a sign-in to another broker that is refused, or whose
// reply is lost, leaves the logged-in broker connected, and the page shows
// what the server holds rather than what it assumed. STATE-01: the old
// session's capabilities are never applied after a switch, even by an
// answer still in flight.
describe('Switching truthfully', () => {
  beforeEach(() => {
    list = brokerList([
      { ...saved(brokerEntry('zerodha', 'Zerodha', 'redirect'), {}), active: true },
      saved(brokerEntry('fivepaisaxts', '5 Paisa (XTS)', 'saved_keys'), {}),
    ])
  })

  async function switchToXts() {
    fireEvent.click(
      (await card('fivepaisaxts')).getByRole('button', { name: 'Login with 5 Paisa (XTS)' })
    )
    fireEvent.click(await screen.findByRole('button', { name: 'Log in to 5 Paisa (XTS)' }))
  }

  it('keeps the logged-in broker when the reply is lost, and says so', async () => {
    serve({ loseSignInReply: true })
    renderPage()
    await switchToXts()
    expect(
      await (await card('fivepaisaxts')).findByText('Could not reach OpenAlgo. Try again.')
    ).toBeInTheDocument()
    expect(assigned).toEqual([])
    // Read again: the server still has Zerodha logged in.
    await waitFor(() => expect(listReads).toBeGreaterThan(1))
    expect((await card('zerodha')).getByText('Active')).toBeInTheDocument()
    expect(
      (await card('fivepaisaxts')).getByRole('button', { name: 'Login with 5 Paisa (XTS)' })
    ).toBeEnabled()
  })

  it("never applies the old broker's capabilities after the switch", async () => {
    let answerOld: (r: Response) => void = () => {}
    serve({
      capabilities: () =>
        new Promise<Response>((resolve) => {
          answerOld = resolve
        }),
    })
    // A request for Zerodha's capabilities is still in flight (AuthSync).
    const pending = useBrokerStore.getState().fetchCapabilities()
    renderPage()
    await switchToXts()
    await waitFor(() => expect(assigned).toEqual(['/dashboard']))
    answerOld(
      new Response(
        JSON.stringify({
          status: 'success',
          data: {
            broker_name: 'zerodha',
            broker_type: 'IN_stock',
            supported_exchanges: ['NSE'],
            leverage_config: false,
          },
        })
      )
    )
    await pending
    expect(useBrokerStore.getState().capabilities).toBeNull()
  })
})

// Switching is enforced: Live automation or unsettled orders on the
// logged-in broker refuse it, and the card says why instead of a confirm.
describe('A switch the server refuses', () => {
  it('names what still runs and offers no confirm', async () => {
    const why =
      'Zerodha still has 2 running Live strategies. They are not moved to another broker. First stop the strategies and protection, then switch. Nothing was changed.'
    list = {
      ...brokerList([
        { ...saved(brokerEntry('zerodha', 'Zerodha', 'redirect'), {}), active: true },
        saved(brokerEntry('fyers', 'Fyers', 'redirect'), {}),
      ]),
      switch_refusal: why,
    }
    serve()
    renderPage()
    fireEvent.click((await card('fyers')).getByRole('button', { name: 'Login with Fyers' }))
    expect(await (await card('fyers')).findByText(why)).toBeInTheDocument()
    expect(screen.queryByText('End your Zerodha session?')).toBeNull()
    expect(posts).toEqual([])
    expect(assigned).toEqual([])
  })
})
