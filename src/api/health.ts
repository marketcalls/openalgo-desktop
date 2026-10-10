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
 */
export async function getCurrentMetrics(): Promise<CurrentMetrics> {
  const response = await webClient.get<CurrentMetrics>('/health/api/current')
  return response.data
}

/**
 * Get metrics history
 * Requires authentication
 */
export async function getMetricsHistory(hours = 24): Promise<HistoricalMetric[]> {
  const response = await webClient.get<HistoricalMetric[]>('/health/api/history', {
    params: { hours },
  })
  return response.data
}

/**
 * Get aggregated statistics
 * Requires authentication
 */
export async function getHealthStats(hours = 24): Promise<HealthStats | EmptyHealthStats> {
  const response = await webClient.get<HealthStats | EmptyHealthStats>('/health/api/stats', {
    params: { hours },
  })
  return response.data
}

/**
 * Get active alerts
 * Requires authentication
 */
export async function getActiveAlerts(): Promise<HealthAlert[]> {
  const response = await webClient.get<HealthAlert[]>('/health/api/alerts')
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

/**
 * Export metrics to CSV
 * Requires authentication
 */
export function exportMetricsCSV(hours = 24): string {
  return `/health/export?hours=${hours}`
}
