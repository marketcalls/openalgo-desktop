/**
 * Desktop: the runner page's half of the Stop handshake, end to end on the
 * page side (LOG-02).
 *
 * The answers below are what the app's page channel actually sends after a
 * Stop, recorded in `tests/fixtures/openscript/` and asserted equal to the
 * live answer by the Rust test that produces them
 * (`src-tauri/tests/it/trading_runner.rs`, `a_refused_close_resumes_the_page`
 * and `an_unfilled_exit_leaves_the_close_pending_and_halted`). Here they go
 * through the real HTTP transport into the real Driver, so a change on
 * either side breaks one of the two tests.
 */

import { readFileSync } from 'node:fs'
import { join } from 'node:path'
import {
  canonicalise,
  check,
  DiagnosticBag,
  emit,
  type HostBar,
  loadText,
  parse,
  sourceFile,
} from 'openalgo-script'
import { describe, expect, it } from 'vitest'
import { Driver, type RunSpec } from './driver'
import { httpTransport } from './transport'

const SOURCE = `version 1

strategy("Up close", overlay = true, qty = 1)

if close > open
    buy()
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

function recorded(name: string): unknown {
  return JSON.parse(
    readFileSync(join(process.cwd(), 'tests', 'fixtures', 'openscript', name), 'utf8')
  )
}

/** The app's page routes, answering the recorded Stop and then one bar. */
function app(afterStop: unknown) {
  const intents: unknown[] = []
  const inbox: unknown[] = [
    afterStop,
    // Bar 3 closes up as bar 4 begins: the script buys on bar 3.
    {
      status: 'success',
      messages: [{ seq: 3, kind: 'bars', bars: [bar(3, true), bar(4, false)] }],
      stop: false,
    },
    { status: 'success', messages: [], stop: true },
  ]
  const fetcher = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input)
    const answer = (body: unknown) =>
      new Response(JSON.stringify(body), {
        status: 200,
        headers: { 'Content-Type': 'application/json' },
      })
    if (url.includes('/inbox')) {
      return answer(inbox.shift() ?? { status: 'success', messages: [], stop: true })
    }
    if (url.endsWith('/intents')) {
      intents.push(JSON.parse(String(init?.body)))
      return answer({ status: 'success', sent: 1 })
    }
    return answer({ status: 'success' })
  }) as typeof fetch
  return { fetcher, intents }
}

async function runAfter(fixture: string) {
  const { fetcher, intents } = app(recorded(fixture))
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
  const driver = new Driver(
    httpTransport('openscript_up', 'secret', fetcher),
    loadText,
    spec,
    () => now
  )
  expect(driver.load()).toBe(true)
  await driver.begin([bar(0, false), bar(1, false), bar(2, false)])
  now = T0 + 4 * MIN + 10_000
  await driver.run()
  return intents
}

describe('the Stop handshake, as the app answers it', () => {
  it('a refused Stop resumes the page: the script sends its next order', async () => {
    const intents = await runAfter('inbox_after_refused_stop.json')
    expect(intents).toHaveLength(1)
    expect(intents[0]).toMatchObject({
      confirmed: true,
      intents: [{ kind: 'place', side: 'buy' }],
    })
  })

  it('a pending Stop keeps the page halted: nothing is sent', async () => {
    expect(await runAfter('inbox_after_pending_stop.json')).toHaveLength(0)
  })
})
