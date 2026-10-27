import { screen } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { renderWithQuery } from '@/test/queryRender'

const client = vi.hoisted(() => ({ get: vi.fn(), post: vi.fn() }))
vi.mock('@/api/client', () => ({ webClient: client }))

import { OrderApproval } from './OrderApproval'

const smart = {
  tool_name: 'place_smart_order',
  args: {
    symbol: 'SBIN',
    exchange: 'NSE',
    action: 'BUY',
    quantity: 0,
    position_size: -500,
    product: 'MIS',
  },
  missing: [],
}

describe('OrderApproval', () => {
  beforeEach(() => client.get.mockReset())

  it('shows the order in words, not the tool name and its JSON', () => {
    renderWithQuery(
      <OrderApproval
        requirement={{
          tool_name: 'place_order',
          args: { symbol: 'SBIN', exchange: 'NSE', action: 'BUY', quantity: 10, product: 'MIS' },
          missing: [],
        }}
      />
    )
    expect(screen.getByText('Buy 10 SBIN on NSE, intraday (MIS), at market.')).toBeInTheDocument()
    expect(screen.queryByText('place_order')).not.toBeInTheDocument()
    expect(screen.queryByText(/"symbol"/)).not.toBeInTheDocument()
    expect(screen.getByText('Symbol')).toBeInTheDocument()
    expect(screen.getByText('SBIN')).toBeInTheDocument()
    expect(client.get).not.toHaveBeenCalled()
  })

  it('shows an account-wide operation with no arguments as a sentence alone', () => {
    const { container } = renderWithQuery(
      <OrderApproval requirement={{ tool_name: 'cancel_all_orders', args: {}, missing: [] }} />
    )
    expect(screen.getByText(/^Cancel every open order on the account/)).toBeInTheDocument()
    expect(container.querySelector('dl')).toBeNull()
  })

  it('shows what a smart order will send from the position held now', async () => {
    client.get.mockResolvedValue({
      data: {
        status: 'success',
        data: { held: 500, target: -500, action: 'SELL', quantity: 1000 },
      },
    })
    renderWithQuery(<OrderApproval requirement={smart} />)
    expect(
      await screen.findByText('Held now: long 500. This sends Sell 1,000 to reach short 500.')
    ).toBeInTheDocument()
    expect(client.get).toHaveBeenCalledWith('/agent/api/orders/preview', {
      params: {
        symbol: 'SBIN',
        exchange: 'NSE',
        product: 'MIS',
        position_size: '-500',
        quantity: '0',
        action: 'BUY',
      },
    })
  })

  it('says when the position held cannot be read', async () => {
    client.get.mockResolvedValue({
      data: { status: 'success', data: { held: null, target: -500 } },
    })
    renderWithQuery(<OrderApproval requirement={smart} />)
    expect(await screen.findByText(/^The position held now could not be read/)).toBeInTheDocument()
  })
})
