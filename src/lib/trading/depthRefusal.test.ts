import { describe, expect, it } from 'vitest'
import { DepthRefusals, refusesDepth } from './depthRefusal'

const refusal = {
  type: 'subscribe',
  status: 'error',
  subscriptions: [
    { symbol: 'SBIN', exchange: 'NSE', status: 'error', message: 'no depth', broker: 'indmoney' },
  ],
  message: 'Subscription processing complete',
}

describe('the terminal without market depth', () => {
  it('reads a refusal for its symbol from a subscribe answer or an INVALID_MODE error', () => {
    expect(refusesDepth(refusal, 'SBIN', 'NSE')).toBe(true)
    expect(refusesDepth(refusal, 'sbin', 'nse')).toBe(true)
    expect(refusesDepth(refusal, 'TCS', 'NSE')).toBe(false)
    expect(
      refusesDepth({ status: 'error', code: 'INVALID_MODE', message: 'x' }, 'SBIN', 'NSE')
    ).toBe(true)
    const ok = {
      type: 'subscribe',
      status: 'success',
      subscriptions: [{ ...refusal.subscriptions[0], status: 'success' }],
    }
    expect(refusesDepth(ok, 'SBIN', 'NSE')).toBe(false)
    expect(refusesDepth({ type: 'auth', status: 'success' }, 'SBIN', 'NSE')).toBe(false)
  })

  it('switches a refused symbol to Quote once, and a new feed forgets it', () => {
    const r = new DepthRefusals()
    expect(r.modeFor('SBIN', 'NSE')).toBe('Depth')
    expect(r.takeRefusal(refusal, 'SBIN', 'NSE')).toBe(true)
    expect(r.takeRefusal(refusal, 'SBIN', 'NSE')).toBe(false)
    expect(r.modeFor('SBIN', 'NSE')).toBe('Quote')
    expect(r.modeFor('TCS', 'NSE')).toBe('Depth')
    r.clear()
    expect(r.modeFor('SBIN', 'NSE')).toBe('Depth')
  })
})
