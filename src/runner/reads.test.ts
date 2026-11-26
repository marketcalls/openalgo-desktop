/**
 * Desktop: a live run's reads of another timeframe, on the engine the page runs.
 *
 * Ported from the web's `test/test_openscript_engine_reads.py` (web 264b3c2a7),
 * whose subject there is the Python runner and here is this page's driver: the
 * same program, the same bars and the same instrument facts, through
 * `Driver.load` and `Driver.begin` on the installed `openalgo-script`.
 *
 * What the web's file pins for its own engine and this one does not need: the
 * runner installing a zone reader before the fold can date a day in
 * Asia/Kolkata. The TypeScript engine reads a zone itself, so a day read was
 * never refused here, and the test below shows it running.
 *
 * Every program is a study, compiled here by the installed compiler. Nothing is
 * sent: the transport records, and a study routes no orders.
 */

import {
  canonicalise,
  check,
  DiagnosticBag,
  emit,
  type HostBar,
  loadText,
  parse,
  sourceFile,
  type Value,
} from 'openalgo-script'
import { describe, expect, it } from 'vitest'
import { Driver, type Loader, type RunSpec, type Transport } from './driver'

const MIN = 60_000
/** 09:15 IST on Tuesday 22 September 2026, the web test's `REGULAR_TUESDAY`. */
const TUESDAY_OPEN = Date.UTC(2026, 8, 22, 3, 45)
const DAY = 86_400_000
const PLATFORM_ZONE = 'Asia/Kolkata'
/** The regular NSE window, as `/openscript/instrument` states it. */
const REGULAR_SESSION = { start: '09:15', end: '15:30', days: [1, 2, 3, 4, 5] }

function compile(name: string, text: string): string {
  const file = sourceFile(name, text)
  const bag = new DiagnosticBag()
  const { program } = emit(file, check(file, parse(file, bag), bag), bag)
  if (program === undefined) throw new Error(`${name} did not compile`)
  return canonicalise(program)
}

function study(title: string, read: string): string {
  return `version 1\nstudy("${title}", overlay = true)\nplot(${read}, "${title}")\n`
}

/** One bar per instant, each closing one point above the bar before it (the web's `rising`). */
function rising(times: readonly number[]): HostBar[] {
  return times.map((time, step) => ({
    time,
    open: 100 + step,
    high: 101 + step,
    low: 99 + step,
    close: 100 + step,
    volume: 10,
  }))
}

/** 09:first to 09:last on a day, one bar a minute (the web's `tuesday_minutes`). */
function minutes(day: number, first: number, last: number): number[] {
  const out: number[] = []
  for (let minute = first; minute <= last; minute += 1) {
    out.push(day + (minute - 15) * MIN)
  }
  return out
}

interface Ran {
  /** The first plot's value on every bar the run executed. */
  values: Value[]
  loaded: boolean
  ended: string[]
}

/**
 * A live run of one program over bars that have all closed, on the real
 * engine, with the facts the app hands the page (`RunSpec.facts`): the
 * instrument record, and `today`, the calendar's window for the day the run
 * starts, as `trading::instrument::facts` writes it beside the record.
 */
async function run(
  program: string,
  bars: readonly HostBar[],
  interval: string,
  instrument: Record<string, unknown>,
  today: Record<string, unknown> | null = null
): Promise<Ran> {
  const ended: string[] = []
  const values: Value[] = []
  const transport: Transport = {
    spec: async () => spec,
    bars: async () => [],
    inbox: async () => ({ messages: [], stop: true }),
    intents: async () => undefined,
    log: async () => undefined,
    ended: async (message) => {
      ended.push(message)
    },
  }
  // The app's answer carries `today` beside the record; the page's type names
  // only the record, which is all it reads.
  const facts = { instrument: { exchange: 'NSE', hasVolume: true, ...instrument }, today }
  const spec: RunSpec = {
    run: {
      id: 'openscript_reads',
      file: 'reads.oscript',
      symbol: 'SBIN',
      exchange: 'NSE',
      interval,
      incarnation: 1,
    },
    program,
    inputs: {},
    facts,
  }
  // Records what each bar plotted, as the web's `plotted` does.
  const recording: Loader = (text, options) => {
    const loaded = loadText(text, options)
    if (loaded.ok) {
      const engine = loaded.engine
      const append = engine.append.bind(engine)
      engine.append = (bar, state, supplied) => {
        const result = append(bar, state, supplied)
        values.push(result.columns[0] ?? null)
        return result
      }
    }
    return loaded
  }
  const last = bars[bars.length - 1]?.time ?? 0
  const driver = new Driver(transport, recording, spec, () => last + DAY)
  const loaded = driver.load()
  if (loaded) await driver.begin(bars)
  await new Promise((resolve) => setTimeout(resolve, 0))
  return { values, loaded, ended }
}

describe('a live run reads another timeframe as its chart and backtest do', () => {
  it('counts a ten minute read from the session open the page is handed', async () => {
    // Web test/test_openscript_engine_reads.py::test_a_ten_minute_read_starts_at_the_session_open
    // From openalgo-script 0.9.0 a minute read is counted from 09:15, as the
    // broker's ten minute candles are. Counted from the epoch, as before, the
    // first bucket ran 09:10 to 09:20 and closed on the 09:20 bar with 09:19's
    // close; from the open it runs 09:15 to 09:25 and closes on the 09:25 bar
    // with 09:24's.
    const program = compile('ten.oscript', study('Ten minute close', 'req.timeframe("10", close)'))
    const times = minutes(TUESDAY_OPEN, 15, 27)
    const closes = times.map((_, step) => 100 + step)

    const opened = await run(program, rising(times), '1m', {
      timezone: PLATFORM_ZONE,
      session: REGULAR_SESSION,
    })
    expect(opened.loaded).toBe(true)
    expect(opened.values).toEqual([...Array(10).fill(null), ...Array(3).fill(closes[9])])

    // The session is what the count starts from: a page handed only the zone
    // counts from midnight there, which puts the bucket back at 09:10. So the
    // live run matches its chart only while the app hands it the session.
    const zoneOnly = await run(program, rising(times), '1m', { timezone: PLATFORM_ZONE })
    expect(zoneOnly.values).toEqual([...Array(5).fill(null), ...Array(8).fill(closes[4])])
  })

  it('dates a day read in the platform zone', async () => {
    // Web test/test_openscript_engine_reads.py::test_a_day_read_on_the_platform_zone_runs_in_that_zone
    // The web refused this read by name until 0.9.0, because its engine's fold
    // could not read the zone. This engine always could: the day closes at the
    // platform's midnight, so nothing on the first day, then that day's last
    // close through the next.
    const program = compile('daily.oscript', study('Daily close', 'req.timeframe("1D", close)'))
    const times = [...minutes(TUESDAY_OPEN, 15, 17), ...minutes(TUESDAY_OPEN + DAY, 15, 17)]

    const ran = await run(program, rising(times), '1m', { timezone: PLATFORM_ZONE })

    expect(ran.values).toEqual([null, null, null, 102, 102, 102])
  })

  it('counts from the regular open on a special-session day, as the chart and the backtest do', async () => {
    // Desktop, a recorded difference from the web (docs/openscript.md,
    // "Differences from OpenAlgo web"). The app hands the page the calendar's
    // window for the start day under `today`, and the page reads only the
    // regular record. The web's runner states a special day's own window as
    // the session of a run started on it, so from 0.9.0 its buckets count
    // from that window's start. Here a thirty minute read on a Sunday evening
    // session from 18:00 (as Muhurat trading is) runs on the 09:15 count:
    // 17:45-18:15, 18:15-18:45, 18:45-19:15, where the web's runs 18:00-18:30
    // and 18:30-19:00.
    const program = compile('half.oscript', study('Half hour close', 'req.timeframe("30", close)'))
    // 18:00 IST on Sunday 8 November 2026, then every five minutes to 18:55.
    const evening = Date.UTC(2026, 10, 8, 12, 30)
    const times = Array.from({ length: 12 }, (_, step) => evening + step * 5 * MIN)

    const ran = await run(
      program,
      rising(times),
      '5m',
      { timezone: PLATFORM_ZONE, session: REGULAR_SESSION },
      {
        date: '2026-11-08',
        open: true,
        isSpecial: true,
        session: { start: '18:00', end: '19:00', days: [7] },
      }
    )

    expect(ran.loaded).toBe(true)
    // The 17:45 bucket closes on the 18:15 bar with 18:10's close, the 18:15
    // one on the 18:45 bar with 18:40's.
    expect(ran.values).toEqual([null, null, null, ...Array(6).fill(102), 108, 108, 108])

    // The record the web's runner states on that day, the special window as
    // the session: the 18:00 bucket closes on the 18:30 bar with 18:25's.
    const web = await run(program, rising(times), '5m', {
      timezone: PLATFORM_ZONE,
      session: { start: '18:00', end: '19:00', days: [7] },
    })
    expect(web.values).toEqual([...Array(6).fill(null), ...Array(6).fill(105)])
  })

  it('refuses at start a read the chart bars cannot fold, naming it and the interval', async () => {
    // Web 264b3c2a7: a chart and a backtest fetch the finer bars such a read
    // needs; a deployed strategy is served none, so a fifteen or three minute
    // read on a ten minute run is refused when it starts, as on the web. The
    // page hands the engine no bars provider, which is what keeps the refusal:
    // given one that answered nothing, the read would be absent on every bar
    // and the strategy would never act, with nothing said.
    const bars = rising(minutes(TUESDAY_OPEN, 15, 45).filter((_, i) => i % 10 === 0))
    const facts = { timezone: PLATFORM_ZONE, session: REGULAR_SESSION }

    const coarser = await run(
      compile('quarter.oscript', study('Quarter', 'req.timeframe("15", close)')),
      bars,
      '10m',
      facts
    )
    expect(coarser.loaded).toBe(false)
    expect(coarser.values).toEqual([])
    expect(coarser.ended).toHaveLength(1)
    // The engine's sentence, whole: '15' alone is inside the code 'OS6015'
    // and would pass with the read left out of the log.
    expect(coarser.ended[0]).toBe(
      'reads.oscript could not be loaded: OS6015 15 is not a whole multiple of 10m.'
    )

    const finer = await run(
      compile('three.oscript', study('Three', 'req.timeframe("3", close)')),
      bars,
      '10m',
      facts
    )
    expect(finer.loaded).toBe(false)
    expect(finer.ended).toEqual([
      'reads.oscript could not be loaded: OS6002 The chart is 10m and the request asks for 3.',
    ])

    // A whole multiple of the run's interval still folds from its own bars.
    const twenty = await run(
      compile('twenty.oscript', study('Twenty', 'req.timeframe("20", close)')),
      bars,
      '10m',
      facts
    )
    expect(twenty.loaded).toBe(true)
    expect(twenty.ended).toEqual([])
  })
})
