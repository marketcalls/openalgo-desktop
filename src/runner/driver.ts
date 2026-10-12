/**
 * Desktop: the OpenScript live runner, one run per page.
 *
 * The desktop runs a deployed strategy on the same `openalgo-script` engine the
 * chart and the backtest use, in a hidden window the app opens for the run, so
 * backtest and live run one engine and cannot drift. This file is the driver
 * the web's Python runner (`openscript_host/openscript_runner.py`) is, on the
 * TypeScript engine:
 *
 * - **The history pass sends nothing and the run begins flat.** Every bar
 *   already closed is replayed to bring the engine to the present; an order the
 *   script decides during it is answered `rejected` here and never leaves the
 *   page, so no position is invented and no old signal is traded.
 * - **Orders are sent from a confirmed bar only.** A moving bar is executed
 *   again on every change at the same index (the engine rolls its state back);
 *   an order a script routes from an unconfirmed execution is answered
 *   `rejected` with the reason, which is what its backtest did.
 * - **The page decides nothing about money.** It holds no key and no session:
 *   it hands each confirmed bar's intents to the app, which turns them into
 *   orders on the run's own side, tags them with the deployment and reports
 *   every order back as a frame. A Stop's closing order is the app's own.
 */

import type {
  Engine,
  EngineHost,
  HostBar,
  LoadResult,
  OrderFrame,
  OrderIntent,
  RoutedEffect,
} from 'openalgo-script'
import { languageInterval } from '@/lib/trading/openscriptIntervals'

/** What the app serves the page about its run. */
export interface RunSpec {
  run: { id: string; file: string; symbol: string; exchange: string; interval: string }
  program: string
  inputs: Record<string, unknown>
  facts: { instrument?: Record<string, unknown> } | null
}

/**
 * `halt` and `resume` are the two halves of a Stop. The app halts the page
 * before it closes the position, so the script sends nothing beside the
 * close. A close that did not happen and left nothing of its own working
 * resumes the page: the run is still holding its position, and its script is
 * what manages it, so it must be able to send its own exits again. A close
 * that left an order working sends no resume: the run stays halted (close
 * pending) until Stop is pressed again or the run is paused.
 */
export type InboxMessage =
  | { seq: number; kind: 'bars'; bars: HostBar[] }
  | { seq: number; kind: 'frame'; frame: OrderFrame }
  | { seq: number; kind: 'halt' }
  | { seq: number; kind: 'resume' }

/** The page's channel to the app. Every call is the run's own. */
export interface Transport {
  spec(): Promise<RunSpec>
  bars(): Promise<HostBar[]>
  inbox(after: number): Promise<{ messages: InboxMessage[]; stop: boolean }>
  intents(intents: readonly OrderIntent[]): Promise<void>
  log(lines: readonly string[]): Promise<void>
  ended(message: string): Promise<void>
}

/** The engine's loader, injected so the driver can be tested on its own. */
export type Loader = (
  text: string,
  options: { settings: Record<string, unknown>; host: EngineHost }
) => LoadResult

const UNIT_MS: Record<string, number> = {
  m: 60_000,
  h: 3_600_000,
  D: 86_400_000,
  W: 604_800_000,
  M: 2_592_000_000,
}

/** One bar's length in milliseconds, from a broker interval code. */
export function intervalMs(code: string): number | null {
  const spelled = languageInterval(code)
  if (spelled === null) return null
  const found = /^([0-9]+)(m|h|D|W|M)?$/.exec(spelled)
  if (found === null) return null
  return Number.parseInt(found[1], 10) * UNIT_MS[found[2] ?? 'm']
}

export const REPLAY_REFUSAL = 'replayed from history, so it was not sent'
export const MOVING_REFUSAL =
  'orders are sent when a bar closes, which is what its backtest did, so an intrabar call is not sent'
export const HALTED_REFUSAL = 'this run is stopping, so nothing further is sent'

export class Driver {
  private engine: Engine | null = null
  private routed: OrderIntent[] = []
  private lastConfirmed: number | null = null
  private moving: HostBar | null = null
  private sending = false
  private halted = false
  private stopped = false
  private currentTime = 0
  private readonly lines: string[] = []
  private readonly barMs: number | null
  private readonly transport: Transport
  private readonly loader: Loader
  private readonly spec: RunSpec
  private readonly now: () => number

  constructor(
    transport: Transport,
    loader: Loader,
    spec: RunSpec,
    now: () => number = () => Date.now()
  ) {
    this.transport = transport
    this.loader = loader
    this.spec = spec
    this.now = now
    this.barMs = intervalMs(spec.run.interval)
  }

  get isStopped(): boolean {
    return this.stopped
  }

  get isSending(): boolean {
    return this.sending
  }

  say(line: string): void {
    this.lines.push(line)
  }

  async flush(): Promise<void> {
    if (this.lines.length === 0) return
    const out = this.lines.splice(0, this.lines.length)
    try {
      await this.transport.log(out)
    } catch {
      // The log is a convenience; a failed write is not worth stopping for.
    }
  }

  /** Load the program. False when the engine refused it. */
  load(): boolean {
    const { run, facts } = this.spec
    const instrument = {
      ...(facts?.instrument ?? {}),
      symbol: run.symbol,
      exchange: run.exchange,
      interval: languageInterval(run.interval) ?? run.interval,
    }
    const self = this
    const host: EngineHost = {
      instrument,
      // The bar's own open time, never the wall clock: two executions of one
      // moving bar must see the same moment.
      get now() {
        return self.currentTime
      },
      route: (effect: RoutedEffect) => {
        for (const intent of effect.intents) this.routed.push(intent)
      },
    }
    const loaded = this.loader(this.spec.program, { settings: this.spec.inputs, host })
    if (!loaded.ok) {
      this.stop(
        `${run.file} could not be loaded: ${loaded.diagnostic.code} ${loaded.diagnostic.message}`
      )
      return false
    }
    this.engine = loaded.engine
    return true
  }

  private stop(message: string): void {
    if (this.stopped) return
    this.stopped = true
    this.say(message)
    void this.flush().then(() => this.transport.ended(message).catch(() => undefined))
  }

  private reject(intents: readonly OrderIntent[], text: string): void {
    for (const intent of intents) {
      this.engine?.deliver({ intentId: intent.intentId, status: 'rejected', filledQty: 0, text })
    }
  }

  /** Whether a bar has closed by the clock. */
  private closedByClock(bar: HostBar): boolean {
    if (this.barMs === null || bar.time === null) return false
    return bar.time + this.barMs <= this.now()
  }

  /** One execution. Sends what a confirmed, live execution routed. */
  private async execute(bar: HostBar, isNew: boolean, confirmed: boolean, supplied?: number) {
    const engine = this.engine
    if (engine === null || this.stopped) return
    this.routed = []
    this.currentTime = bar.time ?? 0
    const state = { isConfirmed: confirmed, isRealtime: this.sending }
    const result = isNew ? engine.append(bar, state, supplied) : engine.update(bar, state)
    const routed = this.routed
    this.routed = []
    if (result.diagnostic !== undefined) {
      this.reject(routed, 'the script stopped on this bar')
      this.stop(
        `${this.spec.run.file} stopped on bar ${result.index}: ${result.diagnostic.code} ${result.diagnostic.message}. Nothing further will be sent for it.`
      )
      return
    }
    for (const alert of result.alerts) {
      this.say(`Alert ${alert.key}: ${alert.title} ${String(alert.message ?? '')}`)
    }
    if (routed.length === 0) return
    if (!this.sending) return this.reject(routed, REPLAY_REFUSAL)
    if (!confirmed) return this.reject(routed, MOVING_REFUSAL)
    if (this.halted) return this.reject(routed, HALTED_REFUSAL)
    try {
      await this.transport.intents(routed)
    } catch {
      this.reject(routed, 'the app could not be reached, so the order was not sent')
    }
  }

  /** Replay history, then begin. */
  async begin(history: readonly HostBar[]): Promise<void> {
    const engine = this.engine
    if (engine === null) return
    const bars = [...history].sort((a, b) => (a.time ?? 0) - (b.time ?? 0))
    engine.history(bars)
    const last = bars[bars.length - 1]
    const forming = last !== undefined && !this.closedByClock(last)
    const closed = forming ? bars.slice(0, -1) : bars
    for (const bar of closed) {
      await this.execute(bar, true, true, bars.length)
      if (this.stopped) return
      this.lastConfirmed = bar.time
    }
    this.sending = true
    this.say(
      `Replayed ${closed.length} bars of history. Nothing was sent for them, and this run begins holding nothing.`
    )
    if (forming && last !== undefined) {
      this.moving = last
      await this.execute(last, true, false)
    }
  }

  /** New or changed bars from the app, oldest first. */
  async onBars(incoming: readonly HostBar[]): Promise<void> {
    const bars = [...incoming]
      .filter(
        (b) => b.time !== null && (this.lastConfirmed === null || b.time > this.lastConfirmed)
      )
      .sort((a, b) => (a.time ?? 0) - (b.time ?? 0))
    for (let i = 0; i < bars.length && !this.stopped; i += 1) {
      const bar = bars[i]
      const confirmed = i < bars.length - 1 || this.closedByClock(bar)
      if (this.moving !== null && this.moving.time === bar.time) {
        this.moving = confirmed ? null : bar
        await this.execute(bar, false, confirmed)
      } else {
        // A newer bar has begun: the one that was moving closed as last seen.
        await this.confirmMoving()
        if (this.stopped) return
        this.moving = confirmed ? null : bar
        await this.execute(bar, true, confirmed)
      }
      if (confirmed) this.lastConfirmed = bar.time
    }
  }

  /** Confirm the moving bar as last seen. */
  async confirmMoving(): Promise<void> {
    const bar = this.moving
    if (bar === null) return
    this.moving = null
    await this.execute(bar, false, true)
    this.lastConfirmed = bar.time
  }

  /** A moving bar whose time is up is closed even when no newer bar came. */
  async onClock(): Promise<void> {
    if (this.moving !== null && this.closedByClock(this.moving)) await this.confirmMoving()
  }

  deliver(frame: OrderFrame): void {
    this.engine?.deliver(frame)
  }

  halt(): void {
    if (!this.halted) this.say('This run was asked to stop. Nothing further is sent.')
    this.halted = true
  }

  /** The close did not happen: the script sends its own orders again. */
  resume(): void {
    if (this.halted) {
      this.say('The position was not closed, so this run carries on and sends its orders again.')
    }
    this.halted = false
  }

  /** Read the inbox until the app says stop. */
  async run(): Promise<void> {
    let after = 0
    while (!this.stopped) {
      let answer: { messages: InboxMessage[]; stop: boolean }
      try {
        answer = await this.transport.inbox(after)
      } catch (e) {
        if (e instanceof RunGone) {
          this.stopped = true
          return
        }
        await new Promise((r) => setTimeout(r, 2000))
        continue
      }
      for (const m of answer.messages) {
        after = Math.max(after, m.seq)
        if (m.kind === 'bars') await this.onBars(m.bars)
        else if (m.kind === 'frame') this.deliver(m.frame)
        else if (m.kind === 'halt') this.halt()
        else if (m.kind === 'resume') this.resume()
      }
      await this.onClock()
      await this.flush()
      if (answer.stop) {
        this.stopped = true
        return
      }
    }
  }
}

/** The app no longer knows this run: the page's work is over. */
export class RunGone extends Error {
  constructor() {
    super('This strategy page is not running any more.')
    this.name = 'RunGone'
  }
}
