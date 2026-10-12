import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import axios from 'axios'
import { readFileSync } from 'node:fs'
import { join } from 'node:path'
import type { ComponentType } from 'react'
import { MemoryRouter } from 'react-router'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { EXPIRY_TEXT } from '@/hooks/useExpiryLookup'
import { axiosFailure } from '@/test/axiosAnswer'

/**
 * Every tool page that shares the Option Chain's expiry lookup settles into
 * a state the trader can act on when the lookup does not produce dates,
 * and its Retry repeats the lookup and recovers (TOOLS-02, gate T05). The
 * web pages leave the dropdown empty and say nothing, or toast once.
 */

const MASTER_NOT_READY = JSON.parse(
  readFileSync(
    join(process.cwd(), 'tests', 'fixtures', 'tools', 'expiries_master_not_ready.json'),
    'utf8'
  )
)
const EXPIRY = '27-OCT-26'

const mocks = vi.hoisted(() => {
  const listing = () => ({ getUnderlyings: vi.fn(), getExpiries: vi.fn() })
  return {
    gex: { ...listing(), getGEXData: vi.fn() },
    gamma: { ...listing(), getGammaDensity: vi.fn() },
    ivSmile: { ...listing(), getIVSmileData: vi.fn() },
    oiTracker: { ...listing(), getOIData: vi.fn(), getMaxPain: vi.fn() },
    oiProfile: { ...listing(), getIntervals: vi.fn(), getProfileData: vi.fn() },
    ivChart: {
      ...listing(),
      getIntervals: vi.fn(),
      getIVData: vi.fn(),
      getDefaultSymbols: vi.fn(),
    },
    volSurface: { getSurfaceData: vi.fn() },
    straddle: { getIntervals: vi.fn(), getStraddleData: vi.fn() },
    custom: { getIntervals: vi.fn(), getLotSize: vi.fn(), simulate: vi.fn() },
  }
})

vi.mock('@/api/gex', () => ({ gexApi: mocks.gex }))
vi.mock('@/api/gamma-density', () => ({ gammaDensityApi: mocks.gamma }))
vi.mock('@/api/iv-smile', () => ({ ivSmileApi: mocks.ivSmile }))
vi.mock('@/api/oi-tracker', () => ({ oiTrackerApi: mocks.oiTracker }))
vi.mock('@/api/oi-profile', () => ({ oiProfileApi: mocks.oiProfile }))
vi.mock('@/api/iv-chart', () => ({ ivChartApi: mocks.ivChart }))
vi.mock('@/api/vol-surface', () => ({ volSurfaceApi: mocks.volSurface }))
vi.mock('@/api/straddle-chart', () => ({ straddleChartApi: mocks.straddle }))
vi.mock('@/api/custom-straddle', () => ({ customStraddleApi: mocks.custom }))
vi.mock('@/lib/Plot2D', () => ({ default: () => null }))
vi.mock('@/lib/Plot3D', () => ({ default: () => null }))
vi.mock('lightweight-charts', () => {
  // Every chart call answers with the same inert stub.
  const stub: unknown = new Proxy(() => stub, {
    get: (_target, key) => (key === 'then' ? undefined : stub),
  })
  return {
    createChart: () => stub,
    LineSeries: {},
    BaselineSeries: {},
    ColorType: { Solid: 'solid' },
    CrosshairMode: { Normal: 0 },
  }
})
vi.mock('@/utils/toast', () => ({
  showToast: {
    error: vi.fn(),
    success: vi.fn(),
    warning: vi.fn(),
    info: vi.fn(),
    dismissAll: vi.fn(),
    dynamic: vi.fn(),
    show: vi.fn(),
  },
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

type ApiMock = Record<string, ReturnType<typeof vi.fn>>

interface PageCase {
  name: string
  load: () => Promise<{ default: ComponentType }>
  /** The API module whose `getExpiries` the page calls. */
  listing: ApiMock
  /** The data request a recovered page starts on its own, if any. */
  data?: () => ReturnType<typeof vi.fn>
}

const PAGES: PageCase[] = [
  {
    name: 'GEX Dashboard',
    load: () => import('./GEXDashboard'),
    listing: mocks.gex,
    data: () => mocks.gex.getGEXData,
  },
  {
    name: 'OI Tracker',
    load: () => import('./OITracker'),
    listing: mocks.oiTracker,
    data: () => mocks.oiTracker.getOIData,
  },
  {
    name: 'OI Range',
    load: () => import('./OIRange'),
    listing: mocks.oiTracker,
    data: () => mocks.oiTracker.getOIData,
  },
  {
    name: 'Max Pain',
    load: () => import('./MaxPain'),
    listing: mocks.oiTracker,
    data: () => mocks.oiTracker.getMaxPain,
  },
  {
    name: 'Gamma Density',
    load: () => import('./GammaDensity'),
    listing: mocks.gamma,
    data: () => mocks.gamma.getGammaDensity,
  },
  {
    name: 'IV Smile',
    load: () => import('./IVSmile'),
    listing: mocks.ivSmile,
    data: () => mocks.ivSmile.getIVSmileData,
  },
  {
    name: 'OI Profile',
    load: () => import('./OIProfile'),
    listing: mocks.oiProfile,
    data: () => mocks.oiProfile.getProfileData,
  },
  { name: 'Vol Surface', load: () => import('./VolSurface'), listing: mocks.oiProfile },
  {
    name: 'Option Greeks',
    load: () => import('./IVChart'),
    listing: mocks.ivChart,
    data: () => mocks.ivChart.getIVData,
  },
  {
    name: 'Straddle Chart',
    load: () => import('./StraddleChart'),
    listing: mocks.oiProfile,
    data: () => mocks.straddle.getStraddleData,
  },
  { name: 'Straddle PnL', load: () => import('./CustomStraddle'), listing: mocks.oiProfile },
]

const busy = 'OpenAlgo is busy. Try again in a few seconds.'

const OUTCOMES = [
  {
    outcome: 'an empty calendar',
    fail: (api: ApiMock) =>
      api.getExpiries.mockResolvedValueOnce({ status: 'success', expiries: [] }),
    text: EXPIRY_TEXT.empty('NIFTY', 'NFO'),
  },
  {
    outcome: 'a JSON error answered with 200',
    fail: (api: ApiMock) =>
      api.getExpiries.mockResolvedValueOnce({ status: 'error', message: 'Lookup failed.' }),
    text: 'Lookup failed.',
  },
  {
    outcome: 'no master contract loaded',
    fail: (api: ApiMock) => api.getExpiries.mockResolvedValueOnce(MASTER_NOT_READY),
    text: MASTER_NOT_READY.message as string,
  },
  {
    outcome: 'a busy refusal',
    fail: async (api: ApiMock) =>
      api.getExpiries.mockRejectedValueOnce(
        await axiosFailure((adapter) => axios.get('/search/api/expiries', { adapter }), 429, {
          status: 'error',
          message: busy,
        })
      ),
    text: busy,
  },
  {
    outcome: 'an unreachable server',
    fail: (api: ApiMock) => api.getExpiries.mockRejectedValueOnce(new Error('Network Error')),
    text: EXPIRY_TEXT.unreachable,
  },
]

beforeEach(() => {
  localStorage.clear()
  for (const api of Object.values(mocks) as ApiMock[]) {
    for (const fn of Object.values(api)) fn.mockReset()
  }
  for (const api of [
    mocks.gex,
    mocks.gamma,
    mocks.ivSmile,
    mocks.oiTracker,
    mocks.oiProfile,
    mocks.ivChart,
  ] as ApiMock[]) {
    api.getUnderlyings.mockResolvedValue({ status: 'success', underlyings: ['NIFTY'] })
    api.getExpiries.mockResolvedValue({ status: 'success', expiries: [EXPIRY] })
  }
  const pending = () => new Promise(() => {})
  for (const fn of [
    mocks.gex.getGEXData,
    mocks.gamma.getGammaDensity,
    mocks.ivSmile.getIVSmileData,
    mocks.oiTracker.getOIData,
    mocks.oiTracker.getMaxPain,
    mocks.oiProfile.getProfileData,
    mocks.ivChart.getIVData,
    mocks.ivChart.getDefaultSymbols,
    mocks.volSurface.getSurfaceData,
    mocks.straddle.getStraddleData,
    mocks.custom.simulate,
  ]) {
    fn.mockImplementation(pending)
  }
  const intervals = { status: 'success', data: { intervals: ['1m', '5m'] } }
  mocks.oiProfile.getIntervals.mockResolvedValue(intervals)
  mocks.ivChart.getIntervals.mockResolvedValue(intervals)
  mocks.straddle.getIntervals.mockResolvedValue(intervals)
  mocks.custom.getIntervals.mockResolvedValue(intervals)
  mocks.custom.getLotSize.mockResolvedValue({ status: 'success', lotsize: 65 })
})

describe.each(PAGES)('$name', ({ load, listing, data }) => {
  it.each(OUTCOMES)('settles on $outcome with Retry, then recovers', async ({ fail, text }) => {
    await fail(listing)
    const { default: Page } = await load()
    render(
      <MemoryRouter>
        <Page />
      </MemoryRouter>
    )
    const alert = await screen.findByRole('alert')
    expect(alert).toHaveTextContent(text)
    expect(screen.queryByTestId('expiry-lookup-loading')).toBeNull()
    if (data) expect(data()).not.toHaveBeenCalled()
    const lookups = listing.getExpiries.mock.calls.length

    fireEvent.click(screen.getByRole('button', { name: 'Retry' }))
    await waitFor(() => expect(screen.queryByRole('alert')).toBeNull())
    expect(listing.getExpiries.mock.calls.length).toBe(lookups + 1)
    expect(listing.getExpiries).toHaveBeenLastCalledWith('NFO', 'NIFTY')
    expect(screen.getAllByText(EXPIRY).length).toBeGreaterThan(0)
    if (data) await waitFor(() => expect(data()).toHaveBeenCalled())
  })
})

describe('the expiry lookup request', () => {
  it('keeps an underlying with & or - whole in the query, on every tool API', async () => {
    const { webClient } = await import('@/api/client')
    const modules = await Promise.all([
      vi.importActual<typeof import('@/api/gex')>('@/api/gex').then((m) => m.gexApi),
      vi
        .importActual<typeof import('@/api/gamma-density')>('@/api/gamma-density')
        .then((m) => m.gammaDensityApi),
      vi.importActual<typeof import('@/api/iv-chart')>('@/api/iv-chart').then((m) => m.ivChartApi),
      vi.importActual<typeof import('@/api/iv-smile')>('@/api/iv-smile').then((m) => m.ivSmileApi),
      vi
        .importActual<typeof import('@/api/oi-profile')>('@/api/oi-profile')
        .then((m) => m.oiProfileApi),
      vi
        .importActual<typeof import('@/api/oi-tracker')>('@/api/oi-tracker')
        .then((m) => m.oiTrackerApi),
    ])
    const seen: string[] = []
    const original = webClient.defaults.adapter
    webClient.defaults.adapter = async (config) => {
      seen.push(config.url ?? '')
      return {
        data: { status: 'success', expiries: [] },
        status: 200,
        statusText: '',
        headers: {},
        config,
      }
    }
    try {
      for (const api of modules) {
        await api.getExpiries('NFO', 'M&M')
        await api.getExpiries('NFO', 'BAJAJ-AUTO')
      }
    } finally {
      webClient.defaults.adapter = original
    }
    expect(new Set(seen)).toEqual(
      new Set([
        '/search/api/expiries?exchange=NFO&underlying=M%26M&instrumenttype=options',
        '/search/api/expiries?exchange=NFO&underlying=BAJAJ-AUTO&instrumenttype=options',
      ])
    )
    expect(seen).toHaveLength(12)
  })
})
