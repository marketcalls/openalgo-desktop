import { act, render, screen } from '@testing-library/react'
import { axe, toHaveNoViolations } from 'jest-axe'
import { afterEach, describe, expect, it } from 'vitest'
import { MarketDataManager } from '@/lib/MarketDataManager'
import { DepthUnavailableNote } from './DepthUnavailableNote'

expect.extend(toHaveNoViolations)

interface Harness {
  handleMessage: (event: MessageEvent) => void
  socket: { readyState: number; send: (m: string) => void; close: () => void } | null
  connectionState: string
}

function deliver(manager: MarketDataManager, message: unknown) {
  ;(manager as unknown as Harness).handleMessage(
    new MessageEvent('message', { data: JSON.stringify(message) })
  )
}

afterEach(() => {
  MarketDataManager.resetInstance()
})

describe('DepthUnavailableNote', () => {
  it('says why the book is empty once the broker refuses depth, and is accessible', async () => {
    const manager = MarketDataManager.getInstance()
    const sent: Array<Record<string, unknown>> = []
    const h = manager as unknown as Harness
    h.socket = {
      readyState: WebSocket.OPEN,
      send: (m) => sent.push(JSON.parse(m)),
      close: () => {},
    }
    h.connectionState = 'authenticated'
    deliver(manager, { type: 'auth', status: 'success', broker: 'indmoney' })
    manager.subscribe('SBIN', 'NSE', 'Depth', () => {})

    const { container } = render(<DepthUnavailableNote symbol="SBIN" exchange="NSE" />)
    expect(screen.queryByRole('status')).toBeNull()

    act(() => {
      deliver(manager, {
        type: 'subscribe',
        status: 'error',
        subscriptions: [{ symbol: 'SBIN', exchange: 'NSE', status: 'error', message: 'no depth' }],
        request_id: sent.find((m) => m.mode === 'Depth')?.request_id,
      })
    })
    expect(screen.getByRole('status')).toHaveTextContent(/does not stream market depth/)
    expect(await axe(container)).toHaveNoViolations()
  })
})
