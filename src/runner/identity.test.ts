/**
 * Desktop: the runner page against a lost answer and another run's frames,
 * on the real engine (NEW-TR-03, NEW-TR-04).
 *
 * These are the two consequences the Codex re-audit of 2026-10-20 executed
 * on the actual Driver (`docs/codex new audit/evidence/trading-driver-probe`):
 * an answer lost after the app accepted a BUY made the page reject it and
 * send a second BUY, and a frame meant for an earlier run of the deployment
 * filled a fresh engine's intent of the same number and drew a SELL.
 */

import {
  canonicalise,
  check,
  DiagnosticBag,
  type Engine,
  emit,
  type HostBar,
  loadText,
  type OrderIntent,
  parse,
  sourceFile,
} from 'openalgo-script'
import { afterEach, describe, expect, it, vi } from 'vitest'
import {
  Driver,
  type InboxMessage,
  type Loader,
  type PageState,
  RETRY_DELAYS_MS,
  Refused,
  RunGone,
  type RunSpec,
  STUCK_AFTER,
  type Transport,
} from './driver'

const SOURCE = `version 1
strategy("Audit", overlay = true, qty = 1)
if close > open
    buy()
if close < open
    close()
`

const T0 = Date.UTC(2026, 9, 5, 4)
const MIN = 60_000

function compile(text: string): string {
  const file = sourceFile('audit.oscript', text)
  const bag = new DiagnosticBag()
  const { program } = emit(file, check(file, parse(file, bag), bag), bag)
  if (program === undefined) throw new Error('did not compile')
  return canonicalise(program)
}

function bar(i: number, up: boolean): HostBar {
  return { time: T0 + i * MIN, open: 100, high: 102, low: 98, close: up ? 101 : 99, volume: 10 }
}

/** One request the page made for intents: the run it named and the intents. */
interface Asked {
  run: number
  ids: number[]
  sides: OrderIntent['side'][]
}

function harness(answer: (call: number) => Error | null) {
  const asked: Asked[] = []
  const logs: string[] = []
  const waiting: boolean[] = []
  const stuck: boolean[] = []
  const transport: Transport = {
    spec: async () => spec,
    bars: async () => [],
    inbox: async () => ({ messages: [], stop: true }),
    intents: async (run: number, intents: readonly OrderIntent[]) => {
      asked.push({ run, ids: intents.map((i) => i.intentId), sides: intents.map((i) => i.side) })
      const failure = answer(asked.length)
      if (failure !== null) throw failure
    },
    log: async (lines: readonly string[], state: PageState) => {
      logs.push(...lines)
      waiting.push(state.waiting)
      stuck.push(state.stuck)
    },
    ended: async () => undefined,
  }
  const spec: RunSpec = {
    run: {
      id: 'same-deployment',
      file: 'audit.oscript',
      symbol: 'SBIN',
      exchange: 'NSE',
      interval: '1m',
      incarnation: 7,
    },
    program: compile(SOURCE),
    inputs: {},
    facts: {
      instrument: { timezone: 'Asia/Kolkata', tickSize: 0.05, lotSize: 1, hasVolume: true },
    },
  }
  let engine: Engine | null = null
  const loader: Loader = (text, options) => {
    const loaded = loadText(text, options)
    if (loaded.ok) engine = loaded.engine
    return loaded
  }
  const driver = new Driver(transport, loader, spec, () => T0 + 10 * MIN)
  const orders = () => (engine as Engine | null)?.orders() ?? []
  return { driver, asked, logs, waiting, stuck, transport, orders }
}

afterEach(() => {
  vi.useRealTimers()
})

describe('a lost answer is not a refusal (NEW-TR-04)', () => {
  it('keeps the order pending and asks again about it, never sending a new entry', async () => {
    vi.useFakeTimers()
    // The app accepted the first request; only its answer was lost.
    const h = harness((n) => (n === 1 ? new Error('response lost after acceptance') : null))
    expect(h.driver.load()).toBe(true)
    await h.driver.begin([])
    await h.driver.onBars([bar(0, true)])
    expect(h.asked).toEqual([{ run: 7, ids: [1], sides: ['buy'] }])
    expect(h.driver.isWaiting).toBe(true)
    expect(h.orders()[0]).toMatchObject({ intentId: 1, status: 'placed' })

    // More confirmed up bars before the app is heard from: nothing new goes.
    await h.driver.onBars([bar(1, true)])
    await h.driver.onBars([bar(2, true)])
    expect(h.asked).toHaveLength(1)

    // The page asks again about the same intent, and the app answers.
    await vi.advanceTimersByTimeAsync(RETRY_DELAYS_MS[0])
    expect(h.asked).toEqual([
      { run: 7, ids: [1], sides: ['buy'] },
      { run: 7, ids: [1], sides: ['buy'] },
    ])
    expect(h.driver.isWaiting).toBe(false)
    expect(h.logs.some((l) => l.includes('did not come back'))).toBe(true)
    expect(h.waiting).toContain(true)
    expect(h.waiting[h.waiting.length - 1]).toBe(false)

    // Its frame arrives; the script's exit is the next order sent.
    h.driver.deliver({
      intentId: 1,
      status: 'filled',
      filledQty: 1,
      avgFillPrice: 100,
      orderRef: 'OID1',
    })
    await h.driver.onBars([bar(3, false)])
    expect(h.asked[2]).toMatchObject({ run: 7, sides: ['sell'] })
    const buys = new Set(h.asked.filter((a) => a.sides[0] === 'buy').flatMap((a) => a.ids))
    expect([...buys]).toEqual([1])
  })

  it('backs off between asks and stops when the app no longer knows the run', async () => {
    vi.useFakeTimers()
    const h = harness((n) => (n <= 4 ? new Error('no answer') : new RunGone()))
    h.driver.load()
    await h.driver.begin([])
    await h.driver.onBars([bar(0, true)])
    const asksAfter = async (ms: number) => {
      await vi.advanceTimersByTimeAsync(ms)
      return h.asked.length
    }
    expect(await asksAfter(RETRY_DELAYS_MS[0] - 1)).toBe(1)
    expect(await asksAfter(1)).toBe(2)
    expect(await asksAfter(RETRY_DELAYS_MS[1])).toBe(3)
    expect(await asksAfter(RETRY_DELAYS_MS[2])).toBe(4)
    expect(await asksAfter(RETRY_DELAYS_MS[3])).toBe(5)
    expect(h.driver.isStopped).toBe(true)
    expect(await asksAfter(60_000)).toBe(5)
    expect(h.asked.every((a) => a.ids.join() === '1')).toBe(true)
  })

  it('after about a minute without an answer the trader is told; the batch stays pending', async () => {
    vi.useFakeTimers()
    const h = harness(() => new Error('no answer'))
    h.driver.load()
    await h.driver.begin([])
    await h.driver.onBars([bar(0, true)])
    const waitedFor = (asks: number) =>
      Array.from(
        { length: asks },
        (_, i) => RETRY_DELAYS_MS[Math.min(i, RETRY_DELAYS_MS.length - 1)]
      ).reduce((a, b) => a + b, 0)
    await vi.advanceTimersByTimeAsync(waitedFor(STUCK_AFTER - 1))
    expect(h.stuck).not.toContain(true)
    await vi.advanceTimersByTimeAsync(waitedFor(STUCK_AFTER) - waitedFor(STUCK_AFTER - 1))
    expect(waitedFor(STUCK_AFTER)).toBeGreaterThanOrEqual(50_000)
    expect(h.stuck[h.stuck.length - 1]).toBe(true)
    expect(h.logs.filter((l) => l.includes('for over a minute'))).toHaveLength(1)
    expect(h.driver.isWaiting).toBe(true)
    // Still asking about the same intent, never sending it anew.
    expect(h.asked.every((a) => a.ids.join() === '1')).toBe(true)
  })

  it('tells the app it is stuck even with no line to log', async () => {
    vi.useFakeTimers()
    const h = harness(() => new Error('no answer'))
    h.driver.load()
    // A page that logs nothing still reports its state.
    h.driver.say = () => undefined
    await h.driver.begin([])
    await h.driver.onBars([bar(0, true)])
    expect(h.waiting).toContain(true)
    await vi.advanceTimersByTimeAsync(120_000)
    expect(h.stuck).toContain(true)
    expect(h.logs).toEqual([])
  })

  it('a page that stops asks nothing more', async () => {
    vi.useFakeTimers()
    const h = harness(() => new Error('no answer'))
    h.driver.load()
    await h.driver.begin([])
    await h.driver.onBars([bar(0, true)])
    h.transport.inbox = async () => ({ messages: [], stop: true })
    await h.driver.run()
    expect(h.driver.isStopped).toBe(true)
    expect(vi.getTimerCount()).toBe(0)
    await vi.advanceTimersByTimeAsync(120_000)
    expect(h.asked).toHaveLength(1)
  })

  it('an answer of refusal from the app rejects the batch, and the script trades on', async () => {
    const h = harness((n) => (n === 1 ? new Refused('Only a confirmed bar sends orders.') : null))
    h.driver.load()
    await h.driver.begin([])
    await h.driver.onBars([bar(0, true)])
    expect(h.driver.isWaiting).toBe(false)
    // The engine takes the rejection with the next bar, and buys again.
    await h.driver.onBars([bar(1, true)])
    expect(h.orders()[0]).toMatchObject({ intentId: 1, status: 'rejected' })
    expect(h.asked[1]).toMatchObject({ ids: [2], sides: ['buy'] })
  })
})

describe('only this run hears its frames (NEW-TR-03)', () => {
  it('a frame naming another run never fills this engine, whatever its intent number', async () => {
    const h = harness(() => null)
    h.driver.load()
    await h.driver.begin([])
    await h.driver.onBars([bar(0, true)])
    const fill = (run: number, seq: number): InboxMessage => ({
      seq,
      incarnation: run,
      kind: 'frame',
      frame: {
        intentId: 1,
        status: 'filled',
        filledQty: 1,
        avgFillPrice: 100,
        orderRef: run === 7 ? 'OID1' : 'OLD-INSTANCE-ORDER',
        time: T0,
      },
    })
    const down = (run: number, seq: number): InboxMessage => ({
      seq,
      incarnation: run,
      kind: 'bars',
      bars: [bar(1, false), bar(2, false)],
    })
    const answers = [
      { messages: [fill(6, 1), down(7, 2)], stop: false },
      { messages: [], stop: true },
    ]
    h.transport.inbox = async () => answers.shift() ?? { messages: [], stop: true }
    await h.driver.run()
    expect(h.orders()[0]).toMatchObject({ intentId: 1, status: 'placed', orderRef: '' })
    expect(h.asked).toHaveLength(1)

    // Its own run's frame is taken, and its exit follows.
    const h2 = harness(() => null)
    h2.driver.load()
    await h2.driver.begin([])
    await h2.driver.onBars([bar(0, true)])
    const own = [
      { messages: [fill(7, 1), down(7, 2)], stop: false },
      { messages: [], stop: true },
    ]
    h2.transport.inbox = async () => own.shift() ?? { messages: [], stop: true }
    await h2.driver.run()
    expect(h2.orders()[0]).toMatchObject({ intentId: 1, status: 'filled', orderRef: 'OID1' })
    expect(h2.asked[1]).toMatchObject({ sides: ['sell'] })
  })
})
