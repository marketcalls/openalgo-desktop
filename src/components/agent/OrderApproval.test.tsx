import { render, screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'
import { OrderApproval } from './OrderApproval'

describe('OrderApproval', () => {
  it('shows the order in words, not the tool name and its JSON', () => {
    render(
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
  })

  it('shows an account-wide operation with no arguments as a sentence alone', () => {
    const { container } = render(
      <OrderApproval requirement={{ tool_name: 'cancel_all_orders', args: {}, missing: [] }} />
    )
    expect(screen.getByText(/^Cancel every open order on the account/)).toBeInTheDocument()
    expect(container.querySelector('dl')).toBeNull()
  })
})
