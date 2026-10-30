/**
 * The Multi-Leg node's manual leg builder: the two readymade spreads, the
 * listed-strike picker's placeholder, and the listed-contract lookup an open
 * leg row makes (parity F-651, F-654, F-657). The other two files mock
 * getOptionStrikes without its arguments; this one records them and reads the
 * query the row registers, so the request a leg sends (its pinned date, else
 * its own or the node's expiry type, and its option type), the key legs share,
 * when the lookup runs and how long its answer is kept are pinned.
 */

import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { useState } from 'react'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import type { OptionStrikesResponse } from '@/api/flow'

interface StrikesRequest {
  underlying: string
  expiry?: string
  expiryType?: string
  optionType?: 'CE' | 'PE'
}

const getOptionStrikes = vi.fn<(request: StrikesRequest) => Promise<OptionStrikesResponse>>()

vi.mock('@/api/flow', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/api/flow')>()
  return { ...actual, getOptionStrikes: (request: StrikesRequest) => getOptionStrikes(request) }
})

const { CustomLegsFields } = await import('./CustomLegsFields')

const LISTING: OptionStrikesResponse = {
  underlying: 'NIFTY',
  exchange: 'NFO',
  expiry: '25AUG26',
  expiries: ['25AUG26', '01SEP26', '29SEP26'],
  resolved: {
    current_week: '25AUG26',
    next_week: '01SEP26',
    current_month: '25AUG26',
    next_month: '29SEP26',
  },
  optionType: 'CE',
  strikes: [
    { strike: 24150, symbol: 'NIFTY25AUG2624150CE', label: 'ITM1' },
    { strike: 24200, symbol: 'NIFTY25AUG2624200CE', label: 'ATM' },
    { strike: 24250, symbol: 'NIFTY25AUG2624250CE', label: 'OTM1' },
  ],
  atm: 24200,
  underlyingLtp: 24219.05,
  underlyingSymbol: 'NIFTY',
}

const ATM_CE = { strikeMode: 'OFFSET', offset: 'ATM', optionType: 'CE', action: 'BUY', quantity: 1 }

/** The last legs array the builder wrote back to the node. */
let saved: Array<Record<string, unknown>> = []

interface Setup {
  legs?: unknown[]
  /** Not current_week, so a leg's own default type cannot pass for the node's. */
  commonExpiryType?: string
  commonAction?: string
  commonQuantity?: number
  underlying?: string
}

function Harness({ setup }: { setup: Setup }) {
  const [legs, setLegs] = useState<unknown[]>(setup.legs ?? [ATM_CE])
  return (
    <CustomLegsFields
      value={legs}
      onChange={(next) => {
        saved = next
        setLegs(next)
      }}
      commonPriceType="MARKET"
      commonProduct="NRML"
      commonExpiryType={setup.commonExpiryType ?? 'next_month'}
      commonAction={setup.commonAction ?? 'BUY'}
      commonQuantity={setup.commonQuantity ?? 1}
      strangleWidth="OTM2"
      underlying={setup.underlying ?? 'NIFTY'}
    />
  )
}

function mount(setup: Setup = {}) {
  // No retry default on this client: the row's own `retry: false` is pinned.
  const client = new QueryClient()
  const user = userEvent.setup()
  render(
    <QueryClientProvider client={client}>
      <Harness setup={setup} />
    </QueryClientProvider>
  )
  return { client, user }
}

/** The listed-contract queries the rows registered, open or not. */
const strikeQueries = (client: QueryClient) =>
  client.getQueryCache().findAll({ queryKey: ['flow', 'option-strikes'] })

/** The one listed-contract query registered, with the options its row gave it. */
function onlyQuery(client: QueryClient) {
  const queries = strikeQueries(client)
  expect(queries).toHaveLength(1)
  return { key: queries[0].queryKey, options: queries[0].observers[0]?.options }
}

const requests = () => getOptionStrikes.mock.calls.map(([request]) => request)

const listed = () => waitFor(() => expect(screen.getByText(/ATM 24200/)).toBeInTheDocument())

/** The row header toggles, in row order. */
const toggles = () => screen.getAllByRole('button').filter((b) => b.hasAttribute('aria-expanded'))

const settle = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms))

beforeEach(() => {
  saved = []
  getOptionStrikes.mockReset()
  getOptionStrikes.mockResolvedValue(LISTING)
})

describe('F-651 loading a readymade spread', () => {
  it.each([
    ['Bull Call Spread', 'CE'],
    ['Bear Put Spread', 'PE'],
  ])(
    '%s writes a bought ATM and a sold OTM2 %s, whatever the node side',
    async (label, optionType) => {
      const { user } = mount({ commonAction: 'SELL', commonQuantity: 2 })
      await user.click(screen.getByLabelText('Load legs from a readymade strategy'))
      await user.click(await screen.findByRole('option', { name: label }))

      expect(saved).toEqual([
        { strikeMode: 'OFFSET', optionType, action: 'BUY', quantity: 2, offset: 'ATM' },
        { strikeMode: 'OFFSET', optionType, action: 'SELL', quantity: 2, offset: 'OTM2' },
      ])
      expect(screen.getByText('2/10')).toBeInTheDocument()
    }
  )
})

describe('F-654 the listed-strike picker', () => {
  it('reads "Pick a listed strike" until a strike is chosen, then stores the one picked', async () => {
    const { user } = mount({
      legs: [{ strikeMode: 'STRIKE', optionType: 'CE', action: 'BUY', quantity: 1 }],
    })
    await listed()
    const picker = screen.getByLabelText('Strike price')
    expect(picker).toHaveAttribute('role', 'combobox')
    expect(picker.textContent).toBe('Pick a listed strike')
    // The toggle under it offers typing, so the placeholder is the only such text.
    expect(screen.getAllByText('Pick a listed strike')).toHaveLength(1)
    expect(
      screen.getByRole('button', { name: 'Type a strike or {{variable}}' })
    ).toBeInTheDocument()

    await user.click(picker)
    await user.click(await screen.findByRole('option', { name: /^24250/ }))
    expect(saved[0]).toMatchObject({ strikeMode: 'STRIKE', strike: 24250 })
    expect(screen.queryByText('Pick a listed strike')).toBeNull()
  })
})

describe('F-657 the listed-contract lookup', () => {
  it("asks for the node's expiry type for a leg that names none, keyed @type", async () => {
    const { client } = mount()
    await listed()
    expect(requests()).toStrictEqual([
      { underlying: 'NIFTY', expiryType: 'next_month', optionType: 'CE' },
    ])
    expect(onlyQuery(client).key).toEqual(['flow', 'option-strikes', 'NIFTY', '@next_month', 'CE'])
  })

  it("asks for the leg's own expiry type over the node's, with its option type", async () => {
    const { client } = mount({ legs: [{ ...ATM_CE, optionType: 'PE', expiryType: 'next_week' }] })
    await listed()
    expect(requests()).toStrictEqual([
      { underlying: 'NIFTY', expiryType: 'next_week', optionType: 'PE' },
    ])
    expect(onlyQuery(client).key).toEqual(['flow', 'option-strikes', 'NIFTY', '@next_week', 'PE'])
  })

  it('asks for a pinned date by the date itself, keyed by the date', async () => {
    const { client } = mount({ legs: [{ ...ATM_CE, expiry: '01SEP26' }] })
    await listed()
    expect(requests()).toStrictEqual([{ underlying: 'NIFTY', expiry: '01SEP26', optionType: 'CE' }])
    expect(onlyQuery(client).key).toEqual(['flow', 'option-strikes', 'NIFTY', '01SEP26', 'CE'])
  })

  it("asks for the node's expiry type for a {{variable}} date, which has no listing", async () => {
    const { client } = mount({ legs: [{ ...ATM_CE, expiry: '{{webhook.expiry}}' }] })
    await listed()
    expect(requests()).toStrictEqual([
      { underlying: 'NIFTY', expiryType: 'next_month', optionType: 'CE' },
    ])
    expect(onlyQuery(client).key).toEqual(['flow', 'option-strikes', 'NIFTY', '@next_month', 'CE'])
  })

  it('looks up only open rows, and legs on one expiry and side share a lookup', async () => {
    const { client, user } = mount({
      legs: [ATM_CE, { ...ATM_CE, offset: 'OTM1' }, { ...ATM_CE, optionType: 'PE' }],
    })
    await listed()
    expect(requests()).toHaveLength(1)
    const queries = strikeQueries(client)
    expect(queries.map((q) => q.queryKey[4])).toEqual(['CE', 'PE'])
    const pe = queries[1]
    expect(pe.observers.map((o) => o.options.enabled)).toEqual([false])

    // The second row asks for the CE listing the first one already holds.
    await user.click(toggles()[1])
    await user.click(toggles()[2])
    await waitFor(() => expect(requests()).toHaveLength(2))
    expect(requests()[1]).toStrictEqual({
      underlying: 'NIFTY',
      expiryType: 'next_month',
      optionType: 'PE',
    })
    expect(pe.observers.map((o) => o.options.enabled)).toEqual([true])
    await settle(50)
    expect(requests()).toHaveLength(2)
  })

  it('asks nothing while the node has no underlying', async () => {
    const { client } = mount({ underlying: '' })
    await settle(50)
    expect(getOptionStrikes).not.toHaveBeenCalled()
    expect(onlyQuery(client).options?.enabled).toBe(false)
    expect(screen.queryByText('Loading listed contracts...')).toBeNull()
  })

  it('keeps a listing for 30 minutes and does not retry a failed lookup', async () => {
    const { client } = mount()
    await listed()
    const { options } = onlyQuery(client)
    expect(options?.staleTime).toBe(30 * 60 * 1000)
    expect(options?.retry).toBe(false)
    expect(options?.enabled).toBe(true)
  })

  it('says at once that a failed lookup leaves the leg typed by hand, after one request', async () => {
    getOptionStrikes.mockRejectedValue(new Error('no broker session'))
    mount()
    // With the client's default of three retries this would take seconds.
    expect(await screen.findByText(/Listed contracts unavailable/)).toBeInTheDocument()
    expect(getOptionStrikes).toHaveBeenCalledTimes(1)
  })
})
