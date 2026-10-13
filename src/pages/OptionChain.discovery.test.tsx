import { readFileSync } from 'node:fs'
import { join } from 'node:path'
import { act, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { AxiosError, type AxiosResponse, type InternalAxiosRequestConfig } from 'axios'
import { MemoryRouter } from 'react-router'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { webClient } from '@/api/client'
import { EXPIRY_LOOKUP_TIMEOUT_MS, EXPIRY_TEXT } from '@/hooks/useExpiryLookup'
import { useAuthStore } from '@/stores/authStore'
import OptionChain from './OptionChain'

/**
 * The real Option Chain page against the session route's answers (TOOLS-01,
 * TOOLS-02; gates T01 and T05). Only the transport is replaced: the page's
 * own API module sends the request, and the chain is polled through the
 * real hook.
 *
 * The Fyers answer is the golden file the Rust test
 * `tools_discovery::fyers_master_fills_the_option_chain_expiry_dropdown`
 * produces from Fyers' raw master through its parser and the session
 * route, so the two ends cannot drift apart.
 */

const fixture = (name: string) =>
  JSON.parse(readFileSync(join(process.cwd(), 'tests', 'fixtures', 'tools', name), 'utf8'))
const FYERS_NIFTY = fixture('fyers_nifty_option_expiries.json')
const MASTER_NOT_READY = fixture('expiries_master_not_ready.json')

const EXPIRY_URL = '/search/api/expiries?exchange=NFO&underlying=NIFTY&instrumenttype=options'

vi.mock('@/hooks/useMarketData', () => ({
  useMarketData: () => ({
    data: new Map(),
    isConnected: false,
    isAuthenticated: false,
    isPaused: false,
    connectionEpoch: 0,
  }),
}))
vi.mock('@/hooks/useSupportedExchanges', () => {
  const exchanges = [{ value: 'NFO', label: 'NFO' }]
  const value = {
    toolsFnoExchanges: exchanges,
    defaultToolsFnoExchange: 'NFO',
    fnoExchanges: exchanges,
    defaultFnoExchange: 'NFO',
    defaultUnderlyings: { NFO: ['NIFTY'] },
    isCrypto: false,
  }
  return { useSupportedExchanges: () => value }
})
vi.mock('@/components/trading', () => ({ PlaceOrderDialog: () => null }))

type Answer =
  | { status: number; data: unknown }
  | { network: true }
  | { never: true }
  | (() => Promise<never>)

let expiryAnswers: Answer[] = []
const requested: string[] = []
const chainBodies: Array<Record<string, unknown>> = []

function respond(config: InternalAxiosRequestConfig, status: number, data: unknown) {
  const response: AxiosResponse = { data, status, statusText: '', headers: {}, config, request: {} }
  if (!config.validateStatus || config.validateStatus(status)) return response
  throw new AxiosError(
    `Request failed with status code ${status}`,
    AxiosError.ERR_BAD_REQUEST,
    config,
    {},
    response
  )
}

const adapter = async (config: InternalAxiosRequestConfig) => {
  const url = config.url ?? ''
  requested.push(url)
  if (url.startsWith('/search/api/underlyings')) {
    return respond(config, 200, { status: 'success', underlyings: ['BANKNIFTY', 'NIFTY'] })
  }
  if (url.startsWith('/search/api/expiries')) {
    const answer = expiryAnswers.length > 1 ? expiryAnswers.shift() : expiryAnswers[0]
    if (!answer) throw new Error('no expiry answer queued')
    if (typeof answer === 'function') return answer()
    if ('network' in answer) {
      throw new AxiosError('Network Error', AxiosError.ERR_NETWORK, config, {})
    }
    if ('never' in answer) return new Promise<never>(() => {})
    return respond(config, answer.status, answer.data)
  }
  return respond(config, 404, {})
}

const originalAdapter = webClient.defaults.adapter

beforeEach(() => {
  localStorage.clear()
  requested.length = 0
  chainBodies.length = 0
  expiryAnswers = []
  webClient.defaults.adapter = adapter
  useAuthStore.setState({ apiKey: '<APIKEY>' })
  vi.stubGlobal(
    'fetch',
    vi.fn((url: string, init?: RequestInit) => {
      if (url === '/api/v1/optionchain') {
        chainBodies.push(JSON.parse(String(init?.body)))
        // The chain stays on its way: the page should show it is loading.
        return new Promise<Response>(() => {})
      }
      return Promise.reject(new Error(`unexpected fetch ${url}`))
    })
  )
})

afterEach(() => {
  webClient.defaults.adapter = originalAdapter
  vi.unstubAllGlobals()
  vi.useRealTimers()
})

function renderPage() {
  return render(
    <MemoryRouter>
      <OptionChain />
    </MemoryRouter>
  )
}

/** The page spins for the chain, not for a lookup that is over. */
function spinning() {
  return document.querySelector('.animate-spin') !== null
}

async function expectChainStarted() {
  await waitFor(() => expect(chainBodies.length).toBeGreaterThan(0))
  expect(chainBodies[0]).toMatchObject({
    underlying: 'NIFTY',
    exchange: 'NFO',
    expiry_date: '13OCT26',
    apikey: '<APIKEY>',
  })
}

describe('Option Chain expiry discovery', () => {
  it('lists the live Fyers dates, selects the first and starts the chain (T01)', async () => {
    expiryAnswers = [{ status: 200, data: FYERS_NIFTY }]
    renderPage()
    await expectChainStarted()
    expect(requested).toContain(EXPIRY_URL)
    expect(screen.getByText('13-OCT-26')).toBeInTheDocument()
    // Waiting for the chain is real work, so the spinner shows.
    expect(spinning()).toBe(true)
    expect(screen.queryByRole('alert')).toBeNull()
  })

  const failures: Array<{ name: string; answer: Answer; title: string; text: string }> = [
    {
      name: 'an empty calendar',
      answer: { status: 200, data: { status: 'success', expiries: [] } },
      title: 'No live expiry dates',
      text: EXPIRY_TEXT.empty('NIFTY', 'NFO'),
    },
    {
      name: 'a JSON error answered with 200',
      answer: { status: 200, data: { status: 'error', message: 'Expiry lookup failed.' } },
      title: 'Expiry dates could not be loaded',
      text: 'Expiry lookup failed.',
    },
    {
      name: 'no master contract loaded',
      answer: { status: 200, data: MASTER_NOT_READY },
      title: 'Master contract not loaded',
      text: MASTER_NOT_READY.message,
    },
    {
      name: 'a refused session (403)',
      answer: { status: 403, data: { status: 'error', message: 'Forbidden' } },
      title: 'Sign in again',
      text: EXPIRY_TEXT.signin,
    },
    {
      name: 'an expired session (401)',
      answer: { status: 401, data: { status: 'error', message: 'Unauthorized' } },
      title: 'Sign in again',
      text: EXPIRY_TEXT.signin,
    },
    {
      name: 'a busy server (429)',
      answer: {
        status: 429,
        data: { status: 'error', message: 'OpenAlgo is busy. Try again in a few seconds.' },
      },
      title: 'Expiry dates could not be loaded',
      text: 'OpenAlgo is busy. Try again in a few seconds.',
    },
    {
      name: 'an unreachable server',
      answer: { network: true },
      title: 'Expiry dates could not be loaded',
      text: EXPIRY_TEXT.unreachable,
    },
  ]

  it.each(failures)(
    'settles on $name with Retry, then recovers (T05)',
    async ({ answer, title, text }) => {
      expiryAnswers = [answer, { status: 200, data: FYERS_NIFTY }]
      renderPage()
      const alert = await screen.findByRole('alert')
      expect(alert).toHaveTextContent(title)
      expect(alert).toHaveTextContent(text)
      expect(spinning()).toBe(false)
      expect(chainBodies).toHaveLength(0)
      if (title === 'Master contract not loaded') {
        expect(screen.getByRole('link', { name: 'Open Master Contract' })).toHaveAttribute(
          'href',
          '/master-contract'
        )
      }
      const lookups = requested.filter((u) => u === EXPIRY_URL).length
      fireEvent.click(screen.getByRole('button', { name: 'Retry' }))
      await expectChainStarted()
      expect(requested.filter((u) => u === EXPIRY_URL).length).toBe(lookups + 1)
      expect(screen.queryByRole('alert')).toBeNull()
      expect(screen.getByText('13-OCT-26')).toBeInTheDocument()
    }
  )

  it('stops waiting for a lookup that never answers (T05)', async () => {
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] })
    expiryAnswers = [{ never: true }, { status: 200, data: FYERS_NIFTY }]
    renderPage()
    await act(async () => {
      await vi.advanceTimersByTimeAsync(10)
    })
    expect(screen.getByTestId('expiry-lookup-loading')).toBeInTheDocument()
    await act(async () => {
      await vi.advanceTimersByTimeAsync(EXPIRY_LOOKUP_TIMEOUT_MS)
    })
    expect(screen.getByRole('alert')).toHaveTextContent(EXPIRY_TEXT.timeout)
    expect(spinning()).toBe(false)
    vi.useRealTimers()
    fireEvent.click(screen.getByRole('button', { name: 'Retry' }))
    await expectChainStarted()
  })

  it('asks for an API key instead of spinning without one (T05)', async () => {
    useAuthStore.setState({ apiKey: null })
    expiryAnswers = [{ status: 200, data: FYERS_NIFTY }]
    renderPage()
    const alert = await screen.findByRole('alert')
    expect(alert).toHaveTextContent('API key needed')
    expect(screen.getByRole('link', { name: 'Open API Key' })).toHaveAttribute('href', '/apikey')
    expect(spinning()).toBe(false)
    expect(chainBodies).toHaveLength(0)
  })
})
