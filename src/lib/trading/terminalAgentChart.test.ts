/**
 * The terminal's two ends of the Assistant panel: the chart context it posts
 * with a turn and the chart commands it applies from one.
 *
 * Desktop addition. The web pins the pure halves (`chartContract.test.ts`) and
 * the panel (`AgentPanel.test.tsx`) but not the terminal between them: the
 * context read off a live pane, and the promise queue that keeps one bad frame
 * from wedging every later turn.
 */
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { AgentChartCommand } from '@/lib/agent/stream'
import { TradingTerminal } from './terminal'

type Fields = Record<string, unknown>

function pane(fields: Fields = {}): TradingTerminal {
  return Object.assign(Object.create(TradingTerminal.prototype), {
    destroyed: false,
    chartCommandQueue: Promise.resolve(),
    ...fields,
  }) as TradingTerminal
}

/** A chart stand-in with a viewport of bars 10.4 to 70.6 at one minute a bar. */
function chart(indicators: { id: string; indicatorId: string; name: string }[] = []) {
  return {
    getVisibleLogicalRange: () => ({ from: 10.4, to: 70.6 }),
    dataLayer: { indexToTimeFloat: (i: number) => 1_700_000_000 + i * 60 },
    indicators: () => indicators,
  }
}

const draw = (cmd: Partial<AgentChartCommand>) => ({ op: 'draw', ...cmd }) as AgentChartCommand

afterEach(() => {
  vi.restoreAllMocks()
})

describe('the chart context a turn carries', () => {
  it('is null when no instrument is on the pane', () => {
    expect(pane({ chart: chart(), sym: null }).chartContext()).toBeNull()
    expect(
      pane({ chart: null, sym: { symbol: 'INFY', exchange: 'NSE' } }).chartContext()
    ).toBeNull()
  })

  it('reports the instrument, viewport, descriptor ids and the two kinds of drawing', () => {
    const terminal = pane({
      chart: chart([{ id: 'ema-1', indicatorId: 'ema', name: 'EMA 20' }]),
      sym: { symbol: 'RELIANCE', exchange: 'NSE' },
      interval: '5m',
      ctype: 'candlestick',
      rawBars: [
        { time: 1, close: 100 },
        { time: 2, close: 101.5 },
      ],
      lastLtp: null,
      // No controller yet: the snapshot restored from storage is what is on screen.
      draw: null,
      drawJson: {
        drawings: [
          {
            id: 'd7',
            tool: 'trend-line',
            points: [{ time: 1, price: 99 }],
            text: { value: 'mine' },
          },
          { id: 'ai:levels:0', tool: 'horizontal-line', points: [{ time: 1, price: 100 }] },
        ],
      },
    })

    expect(terminal.chartContext()).toEqual({
      symbol: 'RELIANCE',
      exchange: 'NSE',
      interval: '5m',
      chart_type: 'candlestick',
      bars_loaded: 2,
      visible_bars: 60,
      visible_from: 1_700_000_624,
      visible_to: 1_700_004_236,
      // No live tick yet, so the last bar's close.
      last_price: 101.5,
      indicators: [{ id: 'ema-1', indicatorId: 'ema', name: 'EMA 20' }],
      drawings: [{ tool: 'trend-line', points: [{ time: 1, price: 99 }], text: 'mine' }],
      agent_groups: ['levels'],
    })
  })

  it('prefers the live price and leaves an unreadable viewport edge empty', () => {
    const c = chart()
    c.dataLayer.indexToTimeFloat = () => Number.NaN
    const context = pane({
      chart: c,
      sym: { symbol: 'INFY', exchange: 'NSE' },
      interval: 'D',
      ctype: 'line',
      rawBars: [{ time: 1, close: 100 }],
      lastLtp: 1520.5,
      draw: null,
      drawJson: { drawings: [] },
    }).chartContext()
    expect(context?.last_price).toBe(1520.5)
    expect(context?.visible_from).toBeNull()
    expect(context?.visible_to).toBeNull()
  })
})

describe('applying chart commands', () => {
  it('applies frames in the order they arrived, one after the other', async () => {
    const order: string[] = []
    let releaseFirst = () => {}
    const terminal = pane()
    const run = vi.spyOn(
      terminal as unknown as { runChartCommands: (c: AgentChartCommand[]) => Promise<void> },
      'runChartCommands'
    )
    run.mockImplementationOnce(async (c) => {
      // The first frame is still attaching the drawing tier when the second lands.
      await new Promise<void>((resolve) => {
        releaseFirst = resolve
      })
      order.push(String(c[0].group))
    })
    run.mockImplementationOnce(async (c) => {
      order.push(String(c[0].group))
    })

    const first = terminal.applyChartCommands([draw({ group: 'levels' })])
    const second = terminal.applyChartCommands([draw({ group: 'zone' })])
    await Promise.resolve()
    expect(order).toEqual([])
    releaseFirst()
    await Promise.all([first, second])
    expect(order).toEqual(['levels', 'zone'])
  })

  it('logs a failed frame and still applies the next turn', async () => {
    const logged = vi.spyOn(console, 'error').mockImplementation(() => {})
    const terminal = pane()
    const run = vi.spyOn(
      terminal as unknown as { runChartCommands: (c: AgentChartCommand[]) => Promise<void> },
      'runChartCommands'
    )
    run.mockRejectedValueOnce(new Error('tier failed to load'))
    run.mockResolvedValueOnce(undefined)

    await expect(terminal.applyChartCommands([draw({ group: 'levels' })])).resolves.toBeUndefined()
    await terminal.applyChartCommands([draw({ group: 'zone' })])
    expect(run).toHaveBeenCalledTimes(2)
    expect(logged).toHaveBeenCalledWith('[trading] chart command', expect.any(Error))
  })

  it('sends indicator ops to the indicator tier and never loads drawings for them', async () => {
    const terminal = pane()
    const internals = terminal as unknown as {
      runIndicatorCommands: (c: AgentChartCommand[]) => Promise<void>
      attachDrawing: () => Promise<void>
    }
    const indicators = vi.spyOn(internals, 'runIndicatorCommands').mockResolvedValue(undefined)
    const attach = vi.spyOn(internals, 'attachDrawing').mockResolvedValue(undefined)

    const add = { op: 'indicator', action: 'add', id: 'alphatrend' } as AgentChartCommand
    await terminal.applyChartCommands([add])
    expect(indicators).toHaveBeenCalledWith([add])
    expect(attach).not.toHaveBeenCalled()
  })

  it('does nothing once the pane is gone', async () => {
    const terminal = pane({ destroyed: true })
    const attach = vi
      .spyOn(terminal as unknown as { attachDrawing: () => Promise<void> }, 'attachDrawing')
      .mockResolvedValue(undefined)
    await terminal.applyChartCommands([draw({ group: 'levels', shapes: [] })])
    expect(attach).not.toHaveBeenCalled()
  })
})
