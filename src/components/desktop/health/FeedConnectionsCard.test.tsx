import { describe, expect, it } from 'vitest'
import type { BrokerFeedLosses, CurrentMetrics, FeedConnection } from '@/api/health'
import { checkA11y } from '@/test/a11y-utils'
import { render, screen } from '@/test/test-utils'
import { FeedConnectionsCard } from './FeedConnectionsCard'
import type { HealthRead } from './useHealthReads'

const NOW = Date.parse('2026-10-26T05:00:00Z')

function connection(extra: Partial<FeedConnection>): FeedConnection {
  return {
    broker: 'kotak',
    state: 'connected',
    connects: 4,
    disconnects: 3,
    stalls: 1,
    last_reconnect: null,
    ...extra,
  }
}

function feed(name: string, c?: FeedConnection): BrokerFeedLosses {
  return {
    name,
    connected: c?.state === 'connected',
    connected_for_s: 60,
    last_update_age_s: 1,
    received_since_connect: 10,
    skipped_since_connect: {},
    skipped_total_since_connect: 0,
    connection: c,
  }
}

function read(feeds: BrokerFeedLosses[]): HealthRead<CurrentMetrics> {
  const data: CurrentMetrics = {
    timestamp: '2026-10-26T10:30:00+05:30',
    fd: { count: 1, limit: 10, usage_percent: 10, status: 'pass' },
    memory: { rss_mb: 1, vms_mb: 1, percent: 1, available_mb: 1, swap_mb: 0, status: 'pass' },
    database: { total: 1, connections: {}, status: 'pass' },
    websocket: { total: 0, connections: {}, total_symbols: 0, status: 'pass' },
    threads: { count: 1, stuck: 0, status: 'pass' },
    overall_status: 'pass',
    market_data: {
      status: 'pass',
      broker_feeds: feeds,
      feed_clients: { coalesced: 0, dropped: 0, skipped: 0 },
    },
  }
  return { data, readAt: NOW, failedAt: null }
}

/** The value beside a row's label. */
function rowValue(label: string): string | null {
  const term = screen.getByText(label, { selector: 'dt' })
  return term.parentElement?.querySelector('dd')?.textContent ?? null
}

function cardOf(title: string): HTMLElement {
  const found = screen.getByText(title).closest<HTMLElement>('[data-slot="card"]')
  if (!found) throw new Error(`no card titled ${title}`)
  return found
}

describe('Broker feed connections card', () => {
  it('says when and why a feed last reconnected, and how often it did', async () => {
    const quiet = connection({
      last_reconnect: { reason: 'silent', at: new Date(NOW - 2 * 60_000).toISOString() },
    })
    const { container } = render(
      <FeedConnectionsCard read={read([feed('market_data', quiet)])} now={NOW} />
    )
    expect(rowValue('Broker market data')).toBe('Connected')
    expect(rowValue('Reconnects')).toBe('3 (1 after it went quiet)')
    expect(rowValue('Last reconnect')).toBe('10:28:00 am IST: the broker sent nothing for too long')
    const card = cardOf('Broker Feed Connections')
    expect(card.className).toMatch(/yellow/)
    expect(screen.getByRole('status')).toHaveTextContent(
      'The market data feed reconnected at 10:28:00 am IST because the broker sent nothing for too long; prices paused until it was back. If this keeps happening, check your internet connection.'
    )
    await checkA11y(container)
  })

  it('stays calm once the last reconnect is long past', () => {
    const old = connection({
      last_reconnect: { reason: 'closed', at: new Date(NOW - 3 * 60 * 60_000).toISOString() },
    })
    render(<FeedConnectionsCard read={read([feed('market_data', old)])} now={NOW} />)
    expect(cardOf('Broker Feed Connections').className).not.toMatch(/yellow|red/)
    expect(screen.queryByRole('status')).toBeNull()
    expect(rowValue('Last reconnect')).toBe(
      '07:30:00 am IST: the broker or the network closed the connection'
    )
  })

  it('tells the trader to log in again when the broker refused the feed', () => {
    const refused = connection({ state: 'auth_failed' })
    render(<FeedConnectionsCard read={read([feed('market_data', refused)])} now={NOW} />)
    expect(cardOf('Broker Feed Connections').className).toMatch(/red/)
    expect(rowValue('Broker market data')).toBe('Refused by the broker')
    expect(screen.getByRole('status')).toHaveTextContent(
      'The broker refused its market data feed, so it is not streaming. Log in to your broker again.'
    )
  })

  it('warns while a feed is reconnecting', () => {
    const now = connection({
      state: 'reconnecting',
      last_reconnect: {
        reason: 'heartbeat_unanswered',
        at: new Date(NOW - 5_000).toISOString(),
      },
    })
    render(<FeedConnectionsCard read={read([feed('order_updates', now)])} now={NOW} />)
    expect(screen.getByRole('status')).toHaveTextContent(
      'The order update feed is reconnecting because the broker did not answer a keep-alive check; order updates pause until it is back.'
    )
  })

  it('draws nothing when health carries no connection data', () => {
    const { container } = render(
      <FeedConnectionsCard read={read([feed('market_data')])} now={NOW} />
    )
    expect(container).toBeEmptyDOMElement()
  })
})
