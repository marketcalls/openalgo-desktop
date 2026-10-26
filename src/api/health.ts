/**
 * Health Monitoring API Client
 * Industry-standard health check endpoints
 */

import { webClient } from './client'

/**
 * "unknown": nothing measured it, or the latest sample is missing, unreadable
 * or older than two sampling periods. Never shown as a pass.
 */
export type HealthLevel = 'pass' | 'warn' | 'fail' | 'unknown'

/** How fresh the sample behind a status is (desktop server). */
export interface SampleFreshness {
  /** Seconds since the latest sample was taken; null when there is none. */
  sample_age_s?: number | null
  /** Why the status is "unknown". */
  reason?: string
}

export interface HealthStatus extends SampleFreshness {
  status: HealthLevel
  version?: string
  serviceId?: string
  description?: string
}

export interface HealthCheck extends HealthStatus {
  checks?: {
    'database:connectivity'?: Array<{
      componentId: string
      status: 'pass' | 'fail'
      time: string
    }>
    'system:file-descriptors'?: Array<{
      componentId: string
      status: 'pass' | 'warn' | 'fail'
      observedValue: number
      observedUnit: string
      time: string
    }>
    'system:memory'?: Array<{
      componentId: string
      status: 'pass' | 'warn' | 'fail'
      observedValue: number
      observedUnit: string
      time: string
    }>
  }
}

export interface CurrentMetrics extends SampleFreshness {
  timestamp: string
  fd: {
    count: number
    limit: number
    usage_percent: number
    status: HealthLevel
  }
  memory: {
    rss_mb: number
    vms_mb: number
    percent: number
    available_mb: number
    swap_mb: number
    status: HealthLevel
  }
  database: {
    total: number
    connections: Record<string, number>
    status: HealthLevel
    /** Desktop: each data file's size in MB. */
    sizes_mb?: Record<string, number> | null
    /** Desktop: how the data files grew since the oldest sample kept. */
    growth?: DataGrowth
  }
  websocket: {
    total: number
    connections: Record<string, { count: number; symbols: number }>
    total_symbols: number
    status: HealthLevel
  }
  threads: {
    count: number
    stuck: number
    details?: Array<{ id: number | null; name: string; daemon: boolean; alive: boolean }>
    status: HealthLevel
  }
  processes?: Array<{
    pid: number | null
    name: string
    rss_mb: number
    vms_mb: number
    memory_percent: number
  }>
  overall_status: HealthLevel
  /** Desktop: the order reconciler and the event bus (`order_facts_json`). */
  order_facts?: OrderFacts
  /** Desktop: records that could not be saved (`record_loss::json`). */
  records?: RecordLosses
  /** Desktop: prices for strategy risk management (`strategy_ticks::json`). */
  strategy_ticks?: StrategyTicks
  /** Desktop: market data skipped per feed and to apps on 8765 (`feed_losses::json`). */
  market_data?: MarketDataLosses
}

/** Sizes and growth of one data file, or of all of them. */
export interface StoreGrowth {
  size_mb: number | null
  growth_mb: number | null
  /** Null over less than a day of samples. */
  per_day_mb: number | null
}

/** Growth from the oldest sample in a window to the newest. */
export interface GrowthWindow {
  window_hours: number
  since: string | null
  stores: Record<string, StoreGrowth>
  total: StoreGrowth
}

/** Over every sample kept (up to 7 days), and over the last day. */
export interface DataGrowth extends GrowthWindow {
  last_day?: GrowthWindow
}

export interface OrderFacts {
  status: 'pass' | 'fail'
  gaps: Array<{ mode: string; cause: string; since: string }>
  running: boolean
  passes: number
  book_reads: number
  book_read_failures: number
  book_reads_discarded: number
  gaps_opened: number
  gaps_closed: number
  unconfirmed_orders: number
  bus?: {
    published: number
    dropped: number
    critical_dropped: number
    closed_sends: number
    panics: number
    critical_panics: number
    stale_dropped: number
    relay_lagged: number
  }
  critical_queues?: Array<{ name: string; depth: number; capacity: number }>
}

export interface RecordLosses {
  status: 'pass' | 'warn'
  order_logs_dropped: number
  order_logs_failed: number
  monitoring_dropped: number
  monitoring_lost: number
  monitoring_retried_batches: number
}

export interface StrategyTicks {
  status: 'pass' | 'warn'
  ticks_queued: number
  ticks_folded: number
  ticks_missed: number
  /** Strategies with an exit or stop decided and not yet placed. */
  exits_waiting: number
  /** Seconds the oldest of those has waited; null when none waits. */
  oldest_exit_wait_s: number | null
  /** The oldest has waited long enough to warn about (15 s). */
  exits_slow: boolean
}

export interface BrokerFeedLosses {
  name: string
  connected: boolean
  connected_for_s: number | null
  last_update_age_s: number | null
  received_since_connect: number
  /** Per reader: strategy, scalping, sandbox, feed_clients, order_relay. */
  skipped_since_connect: Record<string, number>
  skipped_total_since_connect: number
  /** Per reader, since OpenAlgo started: a reconnect does not reset it. */
  skipped_since_start?: Record<string, number>
  skipped_total_since_start?: number
  /** Desktop: how the feed's connection held up (`feed_connections::json`). */
  connection?: FeedConnection
}

/**
 * A broker feed's connection since OpenAlgo started, or since another
 * broker's feed was replaced by this one.
 */
export interface FeedConnection {
  /** The broker of the latest connection; null before the first. */
  broker: string | null
  state: 'disconnected' | 'connecting' | 'connected' | 'reconnecting' | 'auth_failed'
  connects: number
  /** Connections that ended, or attempts that failed, and were tried again. */
  disconnects: number
  /** Of those, the ones that ended because the broker went quiet. */
  stalls: number
  /** Why the latest reconnect happened (`DropCause`) and when (RFC 3339). */
  last_reconnect: { reason: string; at: string } | null
}

export interface MarketDataLosses {
  status: 'pass' | 'warn'
  broker_feeds: BrokerFeedLosses[]
  feed_clients: { coalesced: number; dropped: number; skipped: number }
}

export interface HistoricalMetric {
  timestamp: string
  fd_count: number
  memory_rss_mb: number
  db_connections: number
  ws_connections: number
  threads: number
  overall_status: HealthLevel
}

export interface HealthStats {
  total_samples: number
  time_period_hours: number
  fd: {
    current: number
    avg: number
    min: number
    max: number
    fail_count: number
    warn_count: number
  }
  memory: {
    current_mb: number
    avg_mb: number
    min_mb: number
    max_mb: number
    fail_count: number
    warn_count: number
  }
  database: {
    current: number
    avg: number
    min: number
    max: number
  }
  websocket: {
    current: number
    avg: number
    min: number
    max: number
  }
  threads: {
    current: number
    avg: number
    min: number
    max: number
  }
  status?: {
    overall?: { pass: number; warn: number; fail: number }
    fd?: { warn: number; fail: number }
    memory?: { warn: number; fail: number }
    database?: { warn: number; fail: number }
    websocket?: { warn: number; fail: number }
    threads?: { warn: number; fail: number }
  }
}

/** The stats answer for a window that holds no samples: every group is empty. */
export interface EmptyHealthStats {
  total_samples: 0
  time_period_hours: number
  fd: Record<string, never>
  memory: Record<string, never>
  database: Record<string, never>
  websocket: Record<string, never>
  threads: Record<string, never>
  status?: Record<string, never>
}

export function hasHealthSamples(stats: HealthStats | EmptyHealthStats): stats is HealthStats {
  return stats.total_samples > 0
}

export interface HealthAlert {
  id: number
  timestamp: string
  alert_type: string
  severity: 'warn' | 'fail'
  metric_name: string
  metric_value: number
  threshold_value: number
  message: string
  acknowledged: boolean
  resolved: boolean
  /**
   * Desktop: true when only the trader settles it (lost records, missed
   * strategy prices, bans); false when OpenAlgo resolves it itself.
   */
  resolvable?: boolean
}

/**
 * Simple health check (for AWS ELB, K8s)
 * No authentication required
 */
export async function getSimpleHealth(): Promise<HealthStatus> {
  const response = await webClient.get<HealthStatus>('/health')
  return response.data
}

/**
 * Detailed health check with DB connectivity
 * No authentication required
 */
export async function getDetailedHealthCheck(): Promise<HealthCheck> {
  const response = await webClient.get<HealthCheck>('/health/check')
  return response.data
}

/**
 * Get current metrics snapshot
 * Requires authentication
 *
 * Desktop: each health read takes the Health page's abort signal, so a read
 * that stalls is given up and its request closed (PROD-PAR-01).
 */
export async function getCurrentMetrics(signal?: AbortSignal): Promise<CurrentMetrics> {
  const response = await webClient.get<CurrentMetrics>('/health/api/current', { signal })
  return response.data
}

/**
 * Get metrics history
 * Requires authentication
 */
export async function getMetricsHistory(
  hours = 24,
  signal?: AbortSignal
): Promise<HistoricalMetric[]> {
  const response = await webClient.get<HistoricalMetric[]>('/health/api/history', {
    params: { hours },
    signal,
  })
  return response.data
}

/**
 * Get aggregated statistics
 * Requires authentication
 */
export async function getHealthStats(
  hours = 24,
  signal?: AbortSignal
): Promise<HealthStats | EmptyHealthStats> {
  const response = await webClient.get<HealthStats | EmptyHealthStats>('/health/api/stats', {
    params: { hours },
    signal,
  })
  return response.data
}

/**
 * Get active alerts
 * Requires authentication
 */
export async function getActiveAlerts(signal?: AbortSignal): Promise<HealthAlert[]> {
  const response = await webClient.get<HealthAlert[]>('/health/api/alerts', { signal })
  return response.data
}

/**
 * Acknowledge an alert
 * Requires authentication
 */
export async function acknowledgeAlert(alertId: number): Promise<void> {
  await webClient.post(`/health/api/alerts/${alertId}/acknowledge`)
}

/**
 * Resolve an alert
 * Requires authentication
 */
export async function resolveAlert(alertId: number): Promise<void> {
  await webClient.post(`/health/api/alerts/${alertId}/resolve`)
}

/** A live order the broker did not confirm and does not show: held for the trader. */
export interface HeldOrder {
  owner: string
  id: string
  symbol: string
  exchange: string
  action: string
  quantity: number
  since: string
  message: string
}

/**
 * Live orders held for the trader's decision (desktop only)
 * Requires authentication
 */
export async function getHeldOrders(signal?: AbortSignal): Promise<HeldOrder[]> {
  const response = await webClient.get<{ status: string; data?: HeldOrder[] }>(
    '/health/api/held-orders',
    { signal }
  )
  // An answer without the list is a failed read, never "none held" (PROD-PAR-01).
  if (response.data.status !== 'success' || !Array.isArray(response.data.data)) {
    throw new Error('The unconfirmed orders could not be read')
  }
  return response.data.data
}

/**
 * The trader checked the broker's books: this held order was not placed
 * Requires authentication
 */
export async function markHeldOrderNotPlaced(owner: string, id: string): Promise<void> {
  await webClient.post('/health/api/held-orders/not-placed', { owner, id })
}

/**
 * Export metrics to CSV
 * Requires authentication
 */
export function exportMetricsCSV(hours = 24): string {
  return `/health/export?hours=${hours}`
}
