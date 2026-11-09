/**
 * Desktop: how each broker feed's connection has held up, from
 * `market_data.broker_feeds[].connection` in /health/api/current: its
 * state now, how often it reconnected and why it last did. A trader who
 * sees prices lag can tell a broker that keeps dropping or going quiet
 * from a slow app. The card turns red when the broker refused the feed,
 * and amber while a feed reconnects or for a while after it did.
 */

import { Fragment } from 'react'
import type { BrokerFeedLosses, CurrentMetrics, FeedConnection, HealthLevel } from '@/api/health'
import { AppCard, FEED_NAMES, Row } from './AppHealthCards'
import { readTime } from './sampleFreshness'
import type { HealthRead } from './useHealthReads'

/** What ended the connection, as `DropCause::name` reports it. */
const REASONS: Record<string, string> = {
  connect_failed: 'OpenAlgo could not reach the broker',
  closed: 'the broker or the network closed the connection',
  silent: 'the broker sent nothing for too long',
  data_stopped: 'prices stopped although the connection stayed open',
  heartbeat_unanswered: 'the broker did not answer a keep-alive check',
  not_taking_frames: 'the broker stopped accepting requests',
  login_not_confirmed: 'the broker did not confirm the login in time',
  woke: 'the computer woke from sleep',
}

const STATES: Record<FeedConnection['state'], string> = {
  connected: 'Connected',
  connecting: 'Connecting',
  reconnecting: 'Reconnecting',
  disconnected: 'Not connected',
  auth_failed: 'Refused by the broker',
  connection_limit: 'Too many connections',
  ended_by_broker: 'Stopped by the broker',
}

/** The broker refused or ended the connection: it streams nothing until the trader acts. */
function stoppedState(state: FeedConnection['state']): boolean {
  return state === 'auth_failed' || state === 'connection_limit' || state === 'ended_by_broker'
}

function stopped(c: FeedConnection): boolean {
  return stoppedState(c.state)
}

/** A reconnect this recent still explains a pause the trader just saw. */
export const RECENT_RECONNECT_MS = 15 * 60 * 1000

type Feed = BrokerFeedLosses & { connection: FeedConnection }

/** One pooled connection after the first that carries symbols and is not streaming. */
interface SilentConnection {
  n: number
  instruments: number
  state: FeedConnection['state']
}

/**
 * The feed's further connections that carry symbols and stream nothing:
 * the first connection being up says nothing about them (review R107-16).
 */
function silentConnections(f: Feed): SilentConnection[] {
  return (f.connections?.open ?? [])
    .map((c, i) => ({ n: i + 1, instruments: c.instruments, state: c.state }))
    .filter((c) => c.n > 1 && c.instruments > 0 && c.state !== 'connected')
}

function symbols(n: number): string {
  return `${n.toLocaleString('en-IN')} ${n === 1 ? 'symbol' : 'symbols'}`
}

/** What the trader does about a connection that streams nothing. */
function remedy(state: FeedConnection['state']): string {
  switch (state) {
    case 'auth_failed':
      return 'Log in to your broker again.'
    case 'connection_limit':
      return 'Close other apps streaming on your broker login, then reconnect.'
    case 'ended_by_broker':
      return 'Reconnect your app to OpenAlgo; if it keeps happening, contact your broker.'
    default:
      return 'They resume once it is back.'
  }
}

function reconnectedAt(c: FeedConnection): number | null {
  const at = c.last_reconnect ? Date.parse(c.last_reconnect.at) : Number.NaN
  return Number.isNaN(at) ? null : at
}

function reason(c: FeedConnection): string {
  const r = c.last_reconnect?.reason ?? ''
  return REASONS[r] ?? 'the connection ended'
}

function feedName(f: Feed): string {
  return FEED_NAMES[f.name] ?? f.name
}

/** What a sentence calls each feed, and what stops while it is down. */
const SENTENCE_NAMES: Record<string, { feed: string; paused: string }> = {
  market_data: { feed: 'market data feed', paused: 'prices' },
  order_updates: { feed: 'order update feed', paused: 'order updates' },
  market_depth: { feed: 'market depth feed', paused: 'market depth' },
}

function inSentence(f: Feed): { feed: string; paused: string } {
  return SENTENCE_NAMES[f.name] ?? { feed: 'broker feed', paused: 'updates' }
}

function level(feeds: Feed[], now: number): HealthLevel {
  const silent = feeds.flatMap(silentConnections)
  if (feeds.some((f) => stopped(f.connection))) return 'fail'
  if (silent.some((c) => stoppedState(c.state))) return 'fail'
  const recent = (f: Feed) => {
    const at = reconnectedAt(f.connection)
    return at !== null && now - at < RECENT_RECONNECT_MS
  }
  if (feeds.some((f) => f.connection.state === 'reconnecting' || recent(f))) return 'warn'
  if (silent.length > 0) return 'warn'
  return 'pass'
}

/** What a red or amber card means and what the trader can do. */
function warning(feeds: Feed[], now: number): string | null {
  const notes: string[] = []
  let network = false
  for (const f of feeds) {
    const c = f.connection
    const at = reconnectedAt(c)
    const { feed, paused } = inSentence(f)
    if (c.state === 'auth_failed') {
      notes.push(
        `The broker refused its ${feed}, so it is not streaming. Log in to your broker again.`
      )
    } else if (c.state === 'connection_limit' || c.state === 'ended_by_broker') {
      // The broker's own words and what to do; the feed comes back when an
      // app reconnects to OpenAlgo, without a logout.
      notes.push(c.message ?? `The broker ended its ${feed}, so it is not streaming.`)
    } else if (c.state === 'reconnecting') {
      network = true
      notes.push(
        `The ${feed} is reconnecting because ${reason(c)}; ${paused} pause until it is back.`
      )
    } else if (at !== null && now - at < RECENT_RECONNECT_MS) {
      network = true
      notes.push(
        `The ${feed} reconnected at ${readTime(at, now)} because ${reason(c)}; ${paused} paused until it was back.`
      )
    }
  }
  for (const f of feeds) {
    const { feed, paused } = inSentence(f)
    for (const c of silentConnections(f)) {
      if (!stoppedState(c.state)) network = true
      notes.push(
        `Connection ${c.n} of the ${feed} carries ${symbols(c.instruments)} and is not streaming (${(STATES[c.state] ?? c.state).toLowerCase()}); their ${paused} have stopped. ${remedy(c.state)}`
      )
    }
  }
  if (notes.length === 0) return null
  if (network) {
    notes.push('If this keeps happening, check your internet connection.')
  }
  return notes.join(' ')
}

function reconnects(c: FeedConnection): string {
  const n = c.disconnects.toLocaleString('en-IN')
  return c.stalls > 0 ? `${n} (${c.stalls.toLocaleString('en-IN')} after it went quiet)` : n
}

function lastReconnect(c: FeedConnection, now: number): string {
  const at = reconnectedAt(c)
  return at === null ? 'none' : `${readTime(at, now)}: ${reason(c)}`
}

export function FeedConnectionsCard({
  read,
  now = Date.now(),
}: {
  read: HealthRead<CurrentMetrics>
  now?: number
}) {
  const feeds = (read.data?.market_data?.broker_feeds ?? []).filter(
    (f): f is Feed => f.connection != null
  )
  if (feeds.length === 0) return null
  const stale =
    read.failedAt === null
      ? null
      : `As read at ${readTime(read.readAt, now)}; the refresh at ${readTime(read.failedAt, now)} failed.`
  return (
    <div className="grid gap-4 md:grid-cols-2 lg:grid-cols-3">
      <AppCard
        title="Broker Feed Connections"
        level={level(feeds, now)}
        note="Reconnects are counted since OpenAlgo started, or since this broker's feed began."
        warning={warning(feeds, now)}
        stale={stale}
      >
        {feeds.map((f) => (
          <Fragment key={f.name}>
            <Row label={feedName(f)} value={STATES[f.connection.state] ?? f.connection.state} />
            {(f.connections?.open ?? []).length > 1 &&
              (f.connections?.open ?? []).map((c, i) => (
                <Row
                  key={`${f.name}-${i + 1}`}
                  label={`Connection ${i + 1}`}
                  value={`${STATES[c.state] ?? c.state}, ${symbols(c.instruments)}`}
                />
              ))}
            <Row label="Reconnects" value={reconnects(f.connection)} />
            <Row label="Last reconnect" value={lastReconnect(f.connection, now)} />
          </Fragment>
        ))}
      </AppCard>
    </div>
  )
}
