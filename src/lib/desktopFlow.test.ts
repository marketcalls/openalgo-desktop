import { describe, expect, it } from 'vitest'
import { OPTION_STRATEGIES } from '@/lib/flow/constants'
import {
  FIXED_SIDE_STRATEGIES,
  FLOW_STARTED_IN_BACKGROUND,
  failedRunLog,
  historyIntervalOptions,
  withPanelLevel,
} from './desktopFlow'

const NOW = new Date('2026-10-30T09:20:00Z')

describe('historyIntervalOptions (F-564)', () => {
  it('offers Daily as D, the interval the history API accepts', () => {
    const daily = historyIntervalOptions('5m').find((o) => o.label === 'Daily')
    expect(daily?.value).toBe('D')
    expect(historyIntervalOptions('5m').map((o) => o.value)).not.toContain('1d')
  })

  it('keeps a stored value it does not list, so the field never reads blank', () => {
    const options = historyIntervalOptions('1d')
    expect(options[0]).toEqual({ value: '1d', label: '1d (stored on this node)' })
    expect(options).toHaveLength(6)
  })

  it.each(['5m', 'D', '', undefined, null, 7])('adds nothing for %s', (value) => {
    expect(historyIntervalOptions(value)).toHaveLength(5)
  })
})

describe('FIXED_SIDE_STRATEGIES (F-545)', () => {
  it('names the strategies whose sides ignore the node action', () => {
    expect([...FIXED_SIDE_STRATEGIES].sort()).toEqual([
      'bear_put_spread',
      'bull_call_spread',
      'iron_condor',
    ])
    const known = OPTION_STRATEGIES.map((s) => s.value)
    for (const strategy of FIXED_SIDE_STRATEGIES) expect(known).toContain(strategy)
  })
})

describe('failedRunLog (F-156)', () => {
  const nodeLog = [
    { time: '2026-10-30T09:19:58', message: 'Getting quote for SBIN', level: 'info' },
    { time: '2026-10-30T09:19:59', message: 'Order rejected: margin shortfall', level: 'error' },
  ]

  it('keeps the per-node log a failed run sends back, then the summary', () => {
    const error = Object.assign(new Error('Workflow failed at Place Order'), {
      response: { status: 500, data: { status: 'error', logs: nodeLog } },
    })
    expect(failedRunLog(error, NOW)).toEqual([
      ...nodeLog,
      { time: NOW.toISOString(), message: 'Workflow failed at Place Order', level: 'error' },
    ])
  })

  it('does not repeat a summary the log already ends with', () => {
    const error = Object.assign(new Error('Order rejected: margin shortfall'), {
      response: { data: { logs: nodeLog } },
    })
    expect(failedRunLog(error, NOW)).toEqual(nodeLog)
  })

  it.each([
    ['no reply', new Error('Network Error')],
    ['a reply with no log', Object.assign(new Error('Busy'), { response: { data: {} } })],
    ['an empty log', Object.assign(new Error('Busy'), { response: { data: { logs: [] } } })],
  ])('falls back to the one sentence with %s', (_label, error) => {
    expect(failedRunLog(error, NOW)).toEqual([
      { time: NOW.toISOString(), message: error.message, level: 'error' },
    ])
  })
})

describe('withPanelLevel (F-643)', () => {
  it('shows a Log node warn as a warning and leaves the rest', () => {
    expect(withPanelLevel({ level: 'warn', message: 'x' })).toEqual({
      level: 'warning',
      message: 'x',
    })
    const info = { level: 'info', message: 'y' }
    expect(withPanelLevel(info)).toBe(info)
  })
})

describe('FLOW_STARTED_IN_BACKGROUND (F-207)', () => {
  it('points at a place the trader can look, not an execution history', () => {
    expect(FLOW_STARTED_IN_BACKGROUND).toContain('order book')
    expect(FLOW_STARTED_IN_BACKGROUND).not.toMatch(/history/i)
  })
})
