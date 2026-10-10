import {
  canonicalise,
  check,
  DiagnosticBag,
  emit,
  type HostBar,
  loadText,
  type OrderIntent,
  parse,
  sourceFile,
} from 'openalgo-script'
import { describe, expect, it } from 'vitest'
import {
  Driver,
  type InboxMessage,
  intervalMs,
  REPLAY_REFUSAL,
  type RunSpec,
  type Transport,
} from './driver'
import { readFragment } from './transport'

const SOURCE = `version 1

strategy("Up close", overlay = true, qty = 1)

if close > open
    buy()

if close < open
    close()
`

function compile(text: string): string {
  const file = sourceFile('up.oscript', text)
  const bag = new DiagnosticBag()
  const { program } = emit(file, check(file, parse(file, bag), bag), bag)
  if (program === undefined) throw new Error('did not compile')
  return canonicalise(program)
}

const MIN = 60_000
const T0 = Date.UTC(2026, 9, 5, 4, 0, 0)

function bar(i: number, up: boolean): HostBar {
  const open = 100 + i
  return {
    time: T0 + i * MIN,
    open,
    high: open + 2,
    low: open - 2,
    close: up ? open + 1 : open - 1,
    volume: 10,
  }
}

function harness() {
  const sent: OrderIntent[][] = []
  const logs: string[] = []
  const ended: string[] = []
  const transport: Transport = {
    spec: async () => spec,
    bars: async () => [],
    inbox: async () => ({ messages: [], stop: true }),
    intents: async (i) => {
      sent.push([...i])
    },
    log: async (l) => {
      logs.push(...l)
    },
    ended: async (m) => {
      ended.push(m)
    },
  }
  const spec: RunSpec = {
    run: {
      id: 'openscript_up',
      file: 'up.oscript',
      symbol: 'SBIN',
      exchange: 'NSE',
      interval: '1m',
    },
    program: compile(SOURCE),
    inputs: {},
    facts: {
      instrument: { timezone: 'Asia/Kolkata', tickSize: 0.05, lotSize: 1, hasVolume: true },
    },
  }
  let now = T0 + 3 * MIN + 30_000
  const driver = new Driver(transport, loadText, spec, () => now)
  const setNow = (t: number) => {
    now = t
  }
  return { driver, sent, logs, ended, setNow, transport }
}

describe('runner driver', () => {
  it('reads intervals in the language spelling', () => {
    expect(intervalMs('1m')).toBe(MIN)
    expect(intervalMs('5m')).toBe(5 * MIN)
    expect(intervalMs('D')).toBe(86_400_000)
    expect(intervalMs('30s')).toBeNull()
  })

  it('reads its run and secret from the fragment', () => {
    expect(readFragment('#run=openscript_a&token=abc')).toEqual({
      run: 'openscript_a',
      token: 'abc',
    })
    expect(readFragment('#run=openscript_a')).toBeNull()
  })

  it('replays history without sending, then sends only on a confirmed bar', async () => {
    const h = harness()
    expect(h.driver.load()).toBe(true)
    // Three closed up bars and a fourth still forming.
    await h.driver.begin([bar(0, true), bar(1, true), bar(2, true), bar(3, true)])
    expect(h.sent).toEqual([])
    expect(h.driver.isSending).toBe(true)

    // The forming bar changes: still nothing sent.
    await h.driver.onBars([{ ...bar(3, true), close: 105 }])
    expect(h.sent).toEqual([])

    // The next bar begins, so bar 3 closed: its buy goes out once.
    h.setNow(T0 + 4 * MIN + 10_000)
    await h.driver.onBars([{ ...bar(3, true), close: 105 }, bar(4, false)])
    expect(h.sent).toHaveLength(1)
    expect(h.sent[0][0]).toMatchObject({ kind: 'place', side: 'buy', qty: 1 })

    // Halted: a later confirmed decision is answered here, never sent.
    h.driver.halt()
    h.setNow(T0 + 6 * MIN)
    await h.driver.onBars([bar(5, true)])
    expect(h.sent).toHaveLength(1)
    await h.driver.flush()
    expect(h.logs.some((l) => l.startsWith('Replayed 3 bars'))).toBe(true)
  })

  it('a moving bar whose time is up is closed by the clock', async () => {
    const h = harness()
    h.driver.load()
    await h.driver.begin([bar(0, false), bar(1, false), bar(2, false), bar(3, true)])
    expect(h.sent).toEqual([])
    h.setNow(T0 + 4 * MIN + 1)
    await h.driver.onClock()
    expect(h.sent).toHaveLength(1)
  })

  it('a close that did not happen resumes the run, so its script trades again', async () => {
    // LOG-02: the app halts the page before a Stop closes the position. A
    // close that was refused left the page halted for good, so the script's
    // own stop loss was answered here and never reached the app.
    const run = async (resume: boolean) => {
      const h = harness()
      h.driver.load()
      await h.driver.begin([bar(0, false), bar(1, false), bar(2, false)])
      const control: InboxMessage[] = [{ seq: 1, kind: 'halt' }]
      if (resume) control.push({ seq: 2, kind: 'resume' })
      const answers: { messages: InboxMessage[]; stop: boolean }[] = [
        { messages: control, stop: false },
        // Bar 3 closes up as bar 4 begins: the script buys on bar 3.
        { messages: [{ seq: 3, kind: 'bars', bars: [bar(3, true), bar(4, false)] }], stop: false },
        { messages: [], stop: true },
      ]
      h.transport.inbox = async () => answers.shift() ?? { messages: [], stop: true }
      h.setNow(T0 + 4 * MIN + 10_000)
      await h.driver.run()
      return h.sent
    }
    expect(await run(false)).toHaveLength(0)
    const sent = await run(true)
    expect(sent).toHaveLength(1)
    expect(sent[0][0]).toMatchObject({ kind: 'place', side: 'buy' })
  })

  it('a program the engine refuses ends the run with the reason', async () => {
    const h = harness()
    const broken = new Driver(
      {
        ...({} as Transport),
        log: async () => undefined,
        ended: async (m) => {
          h.ended.push(m)
        },
      },
      loadText,
      {
        run: { id: 'x', file: 'x.oscript', symbol: 'A', exchange: 'NSE', interval: '1m' },
        program: '{}',
        inputs: {},
        facts: null,
      }
    )
    expect(broken.load()).toBe(false)
    await new Promise((r) => setTimeout(r, 0))
    expect(h.ended[0]).toContain('x.oscript could not be loaded')
    expect(REPLAY_REFUSAL).toContain('not sent')
  })
})
