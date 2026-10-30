/**
 * The schedule trigger's market-hours defaults (parity N-marketHours-07):
 * a new Start node is gated to the exchange's session, from 09:15 to 15:15
 * on NSE, as the web's editor writes it. The backend reads these keys from
 * the saved node on every run; a node saved without them runs ungated.
 */

import { describe, expect, it } from 'vitest'
import { DEFAULT_NODE_DATA } from './constants'

describe('start node market-hours defaults', () => {
  it('gate a new schedule to 09:15-15:15 on NSE, as the web', () => {
    const start = DEFAULT_NODE_DATA.start
    expect(start.marketHoursOnly).toBe(true)
    expect(start.marketHoursStart).toBe('09:15')
    expect(start.marketHoursEnd).toBe('15:15')
    expect(start.marketHoursExchange).toBe('NSE')
  })
})
