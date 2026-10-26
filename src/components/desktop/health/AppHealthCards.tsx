/**
 * Desktop: what /health/api/current reports beyond the web's process
 * metrics, one compact card each: the data files and their growth, market
 * data missed per feed and on the way to apps, records not saved, the
 * prices strategy risk management read, and order tracking. A card turns
 * amber or red, with a sentence saying what it means, when something was
 * missed or lost.
 *
 * The checks a trader acts on are never left out (PROD-PAR-01): when health
 * could not be read they show as not known, and when a refresh failed they
 * keep their last figures, marked with when they were read.
 */

import type {
  CurrentMetrics,
  DataGrowth,
  HealthLevel,
  MarketDataLosses,
  OrderFacts,
  RecordLosses,
  StoreGrowth,
  StrategyTicks,
} from '@/api/health'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { cn } from '@/lib/utils'
import { readTime } from './sampleFreshness'
import type { HealthRead } from './useHealthReads'

const STORE_NAMES: Record<string, string> = {
  openalgo: 'Main (symbols, orders, settings)',
  logs: 'Logs',
  sandbox: 'Sandbox',
  historify: 'Historify',
}

/** Who missed market data, as `feed_losses::reader_words` names them. */
const READER_NAMES: Record<string, string> = {
  strategy: 'strategies',
  scalping: 'scalping stops',
  sandbox: 'sandbox orders',
  feed_clients: 'apps on the market data feed',
  order_relay: 'order updates',
}

/**
 * Readers whose skipped update is a price a strategy's stop or target, a
 * scalping stop or a sandbox order's fill never saw: a loss, unlike a slow
 * app getting a newer price instead of every one.
 */
const PRICE_READERS = ['strategy', 'scalping', 'sandbox']

export const FEED_NAMES: Record<string, string> = {
  market_data: 'Broker market data',
  order_updates: 'Broker order updates',
  market_depth: 'Broker market depth',
}

const GAP_CAUSES: Record<string, string> = {
  event_loss: 'an order update could not be delivered inside OpenAlgo',
  relay_lagged: "the broker's order updates came faster than OpenAlgo could relay them",
  order_feed_reconnected: "the broker's order feed reconnected",
}

const MODE_NAMES: Record<string, string> = { live: 'Live', sandbox: 'Sandbox' }

/** The checks shown as not known, never left out, when they cannot be read. */
const TRADER_CHECKS = ['Market Data', 'Order Tracking', 'Records Saved', 'Strategy Prices']

function mb(v: number | null | undefined): string {
  return v == null ? '-' : `${v.toFixed(1)} MB`
}

function signedMb(v: number): string {
  return `${v >= 0 ? '+' : ''}${mb(v)}`
}

function count(n: number, one: string, many: string): string {
  return `${n.toLocaleString('en-IN')} ${n === 1 ? one : many}`
}

export function Row({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex justify-between gap-2">
      <dt className="text-muted-foreground">{label}</dt>
      <dd className="font-medium text-right">{value}</dd>
    </div>
  )
}

/** A card of label and value rows (a description list, so only rows go in
 * it); `note` explains the figures, `warning` says what a miss means, and
 * `stale` says when figures that could not be refreshed were read. */
export function AppCard({
  title,
  level,
  note,
  warning,
  stale,
  children,
}: {
  title: string
  level: HealthLevel
  note?: string | null
  warning?: string | null
  stale?: string | null
  children: React.ReactNode
}) {
  return (
    <Card
      className={cn(
        level === 'warn' && 'border-yellow-500/50',
        level === 'fail' && 'border-red-500/50'
      )}
    >
      <CardHeader className="pb-3">
        <CardTitle className="text-base">{title}</CardTitle>
      </CardHeader>
      <CardContent className="space-y-3">
        <dl className="space-y-2 text-sm">{children}</dl>
        {note && <p className="text-xs text-muted-foreground">{note}</p>}
        {warning && (
          <p
            role="status"
            className={cn(
              'text-xs',
              level === 'fail'
                ? 'text-red-700 dark:text-red-400'
                : 'text-yellow-800 dark:text-yellow-400'
            )}
          >
            {warning}
          </p>
        )}
        {stale && <p className="text-xs text-muted-foreground">{stale}</p>}
      </CardContent>
    </Card>
  )
}

/** A check health could not read: not known, never a pass. */
function NotKnownCard({ title }: { title: string }) {
  return (
    <AppCard
      title={title}
      level="unknown"
      note="OpenAlgo could not read this check. Refresh to try again."
    >
      <Row label="Status" value="Not known" />
    </AppCard>
  )
}

/**
 * A store's size and how it grew: the daily average over a day or more of
 * samples, else the growth itself over the hours there are (a rate from a
 * few hours would stretch one trading session's writes over a whole day).
 */
function sizeAndGrowth(size: number | null, store: StoreGrowth | undefined, hours: number) {
  if (store?.per_day_mb != null) return `${mb(size)} (${signedMb(store.per_day_mb)}/day)`
  if (store?.growth_mb != null && hours >= 1) {
    return `${mb(size)} (${signedMb(store.growth_mb)} in ${Math.round(hours)} h)`
  }
  return mb(size)
}

/** "day", "3 days", "hour", "5 hours": a window's span, after "the last". */
function span(hours: number): string {
  if (hours >= 24) {
    const days = Math.round((hours / 24) * 10) / 10
    return days === 1 ? 'day' : `${days} days`
  }
  const whole = Math.round(hours)
  return whole === 1 ? 'hour' : `${whole} hours`
}

/**
 * The last day's window runs from the oldest sample in the last 24 hours:
 * the whole day once that sample came within a few sampling periods of the
 * day's start, else only the hours the app was running.
 */
const WHOLE_DAY_HOURS = 23.5

/**
 * Which windows the growth figures cover, each named by its own span; the
 * last day is left out when it is the same window as the average's.
 */
function growthNote(growth: DataGrowth | undefined): string | null {
  if (!growth) return null
  const hours = growth.window_hours
  if (hours < 1) return 'Growth shows once an hour of samples is kept.'
  if (hours < 24) {
    return `Growth over the last ${span(hours)}; a daily average shows once a day of samples is kept.`
  }
  const average = span(hours)
  const day = growth.last_day
  const daySpan = day && (day.window_hours >= WHOLE_DAY_HOURS ? 'day' : span(day.window_hours))
  const lastDay =
    day && daySpan && day.total.growth_mb != null && day.window_hours >= 1 && daySpan !== average
      ? ` In the last ${daySpan} all data grew ${signedMb(day.total.growth_mb)}.`
      : ''
  return `Growth per day: the average over the last ${average}.${lastDay}`
}

export function DataFilesCard({
  sizes,
  growth,
  stale,
}: {
  sizes?: Record<string, number> | null
  growth?: DataGrowth
  stale?: string | null
}) {
  const hours = growth?.window_hours ?? 0
  return (
    <AppCard title="Data Files" level="pass" note={growthNote(growth)} stale={stale}>
      {Object.keys(STORE_NAMES).map((name) => {
        const store = growth?.stores[name]
        const size = store?.size_mb ?? sizes?.[name] ?? null
        return (
          <Row key={name} label={STORE_NAMES[name]} value={sizeAndGrowth(size, store, hours)} />
        )
      })}
      <Row
        label="All data"
        value={sizeAndGrowth(growth?.total.size_mb ?? null, growth?.total, hours)}
      />
    </AppCard>
  )
}

/** What one reader skipped on every broker feed since OpenAlgo started. */
function skippedSinceStart(data: MarketDataLosses, reader: string): number {
  return data.broker_feeds.reduce((n, feed) => n + (feed.skipped_since_start?.[reader] ?? 0), 0)
}

export function MarketDataCard({ data, stale }: { data: MarketDataLosses; stale?: string | null }) {
  const priceMisses = PRICE_READERS.map((r) => [r, skippedSinceStart(data, r)] as const).filter(
    ([, n]) => n > 0
  )
  const pricesMissed = priceMisses.reduce((sum, [, n]) => sum + n, 0)
  const orderUpdatesSkipped = skippedSinceStart(data, 'order_relay')
  const apps = data.feed_clients
  const appsMissed = skippedSinceStart(data, 'feed_clients') + apps.dropped + apps.skipped
  const warning =
    [
      pricesMissed > 0 &&
        'Strategies, scalping stops or sandbox orders skipped some prices because they had fallen behind the feed; each acted on the next price. Holding fewer symbols helps.',
      orderUpdatesSkipped > 0 &&
        "Some order updates from the broker were skipped; OpenAlgo re-checks the broker's order book.",
      appsMissed > 0 &&
        'Apps on the market data feed missed some updates because they read too slowly.',
    ]
      .filter(Boolean)
      .join(' ') || null
  const note = `Missed and replaced counts are since OpenAlgo started.${
    apps.coalesced > 0
      ? ' A slow app gets the newest price instead of every update; a replaced update is not a missed one.'
      : ''
  }`
  return (
    <AppCard title="Market Data" level={data.status} note={note} warning={warning} stale={stale}>
      {data.broker_feeds.length === 0 && <Row label="Broker feed" value="Not connected" />}
      {data.broker_feeds.map((feed) => (
        <Row
          key={feed.name}
          label={FEED_NAMES[feed.name] ?? feed.name}
          value={
            feed.connected
              ? `${count(feed.received_since_connect, 'update', 'updates')} since it connected`
              : 'Not connected'
          }
        />
      ))}
      <Row
        label="Prices missed by stops and orders"
        value={
          pricesMissed > 0
            ? `${pricesMissed.toLocaleString('en-IN')}: ${priceMisses
                .map(([r, n]) => `${READER_NAMES[r]} ${n.toLocaleString('en-IN')}`)
                .join(', ')}`
            : 'none'
        }
      />
      {orderUpdatesSkipped > 0 && (
        <Row label="Order updates skipped" value={orderUpdatesSkipped.toLocaleString('en-IN')} />
      )}
      <Row
        label="Missed by apps on the feed"
        value={appsMissed > 0 ? count(appsMissed, 'update', 'updates') : 'none'}
      />
      <Row label="Replaced by a newer price" value={apps.coalesced.toLocaleString('en-IN')} />
    </AppCard>
  )
}

export function RecordsCard({ data, stale }: { data: RecordLosses; stale?: string | null }) {
  const orderLogs = data.order_logs_dropped + data.order_logs_failed
  const monitoring = data.monitoring_dropped + data.monitoring_lost
  return (
    <AppCard
      title="Records Saved"
      level={data.status}
      stale={stale}
      warning={
        data.status === 'warn'
          ? 'Some records were not saved, so a log page may miss entries since OpenAlgo started. Orders themselves were not affected.'
          : null
      }
    >
      <Row
        label="Order log entries"
        value={orderLogs > 0 ? `${orderLogs.toLocaleString('en-IN')} not saved` : 'all saved'}
      />
      <Row
        label="Monitoring records"
        value={monitoring > 0 ? `${monitoring.toLocaleString('en-IN')} not saved` : 'all saved'}
      />
      <Row label="Saves retried" value={data.monitoring_retried_batches.toLocaleString('en-IN')} />
    </AppCard>
  )
}

function strategyTicksWarning(data: StrategyTicks): string | null {
  const notes: string[] = []
  if (data.exits_slow && data.oldest_exit_wait_s !== null) {
    notes.push(
      `A strategy exit decided ${data.oldest_exit_wait_s.toLocaleString('en-IN')} seconds ago is not placed yet: the broker or OpenAlgo's database is answering slowly. Check that strategy's positions with your broker.`
    )
  }
  if (data.ticks_missed > 0) {
    notes.push(
      'Strategy risk management missed some prices: stops and targets act on the next price. Holding fewer symbols helps.'
    )
  }
  return notes.length > 0 ? notes.join(' ') : null
}

export function StrategyTicksCard({ data, stale }: { data: StrategyTicks; stale?: string | null }) {
  return (
    <AppCard
      title="Strategy Prices"
      level={data.status}
      stale={stale}
      warning={strategyTicksWarning(data)}
    >
      <Row label="Prices read" value={data.ticks_queued.toLocaleString('en-IN')} />
      <Row label="Combined while busy" value={data.ticks_folded.toLocaleString('en-IN')} />
      <Row label="Missed" value={data.ticks_missed.toLocaleString('en-IN')} />
      <Row
        label="Strategies with exits waiting"
        value={data.exits_waiting.toLocaleString('en-IN')}
      />
      <Row
        label="Longest exit wait"
        value={
          data.oldest_exit_wait_s === null
            ? 'none'
            : `${data.oldest_exit_wait_s.toLocaleString('en-IN')} s`
        }
      />
    </AppCard>
  )
}

/** Every open gap: its mode, since when, and why. */
function gapReasons(gaps: OrderFacts['gaps']): string {
  return gaps
    .map((g) => {
      const since = Date.parse(g.since)
      const when = Number.isNaN(since) ? '' : ` since ${readTime(since)}`
      return `${MODE_NAMES[g.mode] ?? g.mode}${when}: ${GAP_CAUSES[g.cause] ?? g.cause}`
    })
    .join('; ')
}

export function OrderTrackingCard({ data, stale }: { data: OrderFacts; stale?: string | null }) {
  const lost = (data.bus?.critical_dropped ?? 0) + (data.bus?.critical_panics ?? 0)
  return (
    <AppCard
      title="Order Tracking"
      level={data.status}
      stale={stale}
      warning={
        data.gaps.length > 0
          ? `Some order updates may have been missed (${gapReasons(data.gaps)}). OpenAlgo is re-checking the broker's order book; new strategy entries wait until it has.`
          : null
      }
    >
      <Row label="Order book checks" value={data.running ? 'running' : 'stopped'} />
      <Row label="Order book reads" value={data.book_reads.toLocaleString('en-IN')} />
      <Row label="Reads that failed" value={data.book_read_failures.toLocaleString('en-IN')} />
      <Row label="Unconfirmed orders" value={data.unconfirmed_orders.toLocaleString('en-IN')} />
      <Row label="Order updates lost" value={lost.toLocaleString('en-IN')} />
    </AppCard>
  )
}

/** Every card the sample carries data for; the trader's checks always. */
export function AppHealthCards({ read }: { read: HealthRead<CurrentMetrics> }) {
  const metrics = read.data
  if (!metrics) {
    if (read.failedAt === null) return null
    return (
      <div className="grid gap-4 md:grid-cols-2 lg:grid-cols-3">
        {TRADER_CHECKS.map((title) => (
          <NotKnownCard key={title} title={title} />
        ))}
      </div>
    )
  }
  const stale =
    read.failedAt === null
      ? null
      : `As read at ${readTime(read.readAt)}; the refresh at ${readTime(read.failedAt)} failed.`
  const { database, market_data, records, strategy_ticks, order_facts } = metrics
  return (
    <div className="grid gap-4 md:grid-cols-2 lg:grid-cols-3">
      {(database.growth || database.sizes_mb) && (
        <DataFilesCard sizes={database.sizes_mb} growth={database.growth} stale={stale} />
      )}
      {market_data ? (
        <MarketDataCard data={market_data} stale={stale} />
      ) : (
        <NotKnownCard title="Market Data" />
      )}
      {order_facts ? (
        <OrderTrackingCard data={order_facts} stale={stale} />
      ) : (
        <NotKnownCard title="Order Tracking" />
      )}
      {records ? (
        <RecordsCard data={records} stale={stale} />
      ) : (
        <NotKnownCard title="Records Saved" />
      )}
      {strategy_ticks ? (
        <StrategyTicksCard data={strategy_ticks} stale={stale} />
      ) : (
        <NotKnownCard title="Strategy Prices" />
      )}
    </div>
  )
}
