import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { CurrentMetrics, EmptyHealthStats } from '@/api/health'
import { render, screen } from '@/test/test-utils'
import HealthMonitor from './HealthMonitor'

const api = vi.hoisted(() => ({
  getCurrentMetrics: vi.fn(),
  getMetricsHistory: vi.fn(),
  getHealthStats: vi.fn(),
  getActiveAlerts: vi.fn(),
}))

vi.mock('@/api/health', async (importOriginal) => ({
  ...(await importOriginal<typeof import('@/api/health')>()),
  ...api,
}))

function metrics(overrides: Partial<CurrentMetrics> = {}): CurrentMetrics {
  return {
    timestamp: new Date().toISOString(),
    fd: { count: 120, limit: 10240, usage_percent: 1.2, status: 'pass' },
    memory: {
      rss_mb: 210,
      vms_mb: 900,
      percent: 2.5,
      available_mb: 4000,
      swap_mb: 0,
      status: 'pass',
    },
    database: { total: 3, connections: { openalgo: 2, logs: 1 }, status: 'pass' },
    websocket: { total: 0, connections: {}, total_symbols: 0, status: 'pass' },
    threads: { count: 40, stuck: 0, status: 'pass' },
    processes: [],
    overall_status: 'pass',
    ...overrides,
  }
}

/** What the server answers for a window with no samples (`stats_json` on no rows). */
const NO_SAMPLES: EmptyHealthStats = {
  total_samples: 0,
  time_period_hours: 24,
  fd: {},
  memory: {},
  database: {},
  websocket: {},
  threads: {},
  status: {},
}

describe('Health Monitor', () => {
  beforeEach(() => {
    api.getCurrentMetrics.mockResolvedValue(metrics())
    api.getMetricsHistory.mockResolvedValue([])
    api.getHealthStats.mockResolvedValue(NO_SAMPLES)
    api.getActiveAlerts.mockResolvedValue([])
  })

  afterEach(() => {
    vi.clearAllMocks()
  })

  // The page used to read stats.fd.avg.toFixed on an empty window and crash.
  it('renders an empty statistics window instead of crashing', async () => {
    render(<HealthMonitor />)

    expect(
      await screen.findByText(/No health samples in the last 24 hours yet/)
    ).toBeInTheDocument()
    expect(screen.queryByText('File Descriptor Stats')).not.toBeInTheDocument()
    expect(screen.getByText(/System Status: PASS/)).toBeInTheDocument()
  })

  it('shows the statistics when the window has samples', async () => {
    const group = { current: 3, avg: 2.5, min: 2, max: 3 }
    api.getHealthStats.mockResolvedValue({
      total_samples: 2,
      time_period_hours: 24,
      fd: { current: 120, avg: 118.5, min: 117, max: 120, fail_count: 0, warn_count: 0 },
      memory: {
        current_mb: 210,
        avg_mb: 205.25,
        min_mb: 200.5,
        max_mb: 210,
        fail_count: 0,
        warn_count: 0,
      },
      database: group,
      websocket: group,
      threads: group,
      status: { overall: { pass: 2, warn: 0, fail: 0 } },
    })
    render(<HealthMonitor />)

    expect(await screen.findByText('File Descriptor Stats')).toBeInTheDocument()
    expect(screen.getByText('118.5')).toBeInTheDocument()
    expect(screen.getByText('2/0/0')).toBeInTheDocument()
    expect(screen.queryByText(/No health samples/)).not.toBeInTheDocument()
  })
})
