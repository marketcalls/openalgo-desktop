import { describe, expect, it } from 'vitest'
import type { BrokerFeedLosses, CurrentMetrics } from '@/api/health'
import { checkA11y } from '@/test/a11y-utils'
import { render, screen, within } from '@/test/test-utils'
import { AppHealthCards } from './AppHealthCards'
import type { HealthRead } from './useHealthReads'

function sample(extra: Partial<CurrentMetrics>): CurrentMetrics {
  return {
    timestamp: '2026-10-20T10:00:00+05:30',
    fd: { count: 1, limit: 10, usage_percent: 10, status: 'pass' },
    memory: { rss_mb: 1, vms_mb: 1, percent: 1, available_mb: 1, swap_mb: 0, status: 'pass' },
    database: { total: 1, connections: {}, status: 'pass' },
    websocket: { total: 0, connections: {}, total_symbols: 0, status: 'pass' },
    threads: { count: 1, stuck: 0, status: 'pass' },
    overall_status: 'pass',
    ...extra,
  }
}

/** A sample the page has just read. */
function fresh(extra: Partial<CurrentMetrics>): HealthRead<CurrentMetrics> {
  return { data: sample(extra), readAt: Date.now(), failedAt: null }
}

const growth = {
  window_hours: 24,
  since: '2026-10-19T10:00:00+05:30',
  stores: {
    openalgo: { size_mb: 2.5, growth_mb: 0.1, per_day_mb: 0.1 },
    logs: { size_mb: 40, growth_mb: 6, per_day_mb: 6 },
    sandbox: { size_mb: 1, growth_mb: 0, per_day_mb: 0 },
    historify: { size_mb: 120, growth_mb: null, per_day_mb: null },
  },
  total: { size_mb: 163.5, growth_mb: 6.1, per_day_mb: 6.1 },
}

function feed(
  name: string,
  sinceConnect: Record<string, number>,
  sinceStart: Record<string, number>
): BrokerFeedLosses {
  const sum = (m: Record<string, number>) => Object.values(m).reduce((a, b) => a + b, 0)
  return {
    name,
    connected: true,
    connected_for_s: 60,
    last_update_age_s: 1,
    received_since_connect: 1200,
    skipped_since_connect: sinceConnect,
    skipped_total_since_connect: sum(sinceConnect),
    skipped_since_start: sinceStart,
    skipped_total_since_start: sum(sinceStart),
  }
}

/** The card with this title. */
function card(title: string): HTMLElement {
  const found = screen.getByText(title).closest<HTMLElement>('[data-slot="card"]')
  if (!found) throw new Error(`no card titled ${title}`)
  return found
}

/** The value beside a row's label in a card. */
function rowValue(label: string): string | null {
  const term = screen.getByText(label, { selector: 'dt' })
  return term.parentElement?.querySelector('dd')?.textContent ?? null
}

describe('App health cards', () => {
  it('shows each data file with its size and growth per day', () => {
    render(
      <AppHealthCards
        read={fresh({ database: { total: 1, connections: {}, status: 'pass', growth } })}
      />
    )
    expect(screen.getByText('Data Files')).toBeInTheDocument()
    expect(screen.getByText('40.0 MB (+6.0 MB/day)')).toBeInTheDocument()
    expect(screen.getByText('120.0 MB')).toBeInTheDocument()
    expect(screen.getByText('163.5 MB (+6.1 MB/day)')).toBeInTheDocument()
  })

  // PROD-REL-05: the growth said "per day" without saying over what, and a
  // rate from an hour of samples stretched one session's writes over a day.
  it('says which windows the growth covers: a daily average over days, the last day apart', () => {
    const week = {
      ...growth,
      window_hours: 72,
      last_day: {
        window_hours: 24,
        since: '2026-10-21T10:00:00+05:30',
        stores: {},
        total: { size_mb: 163.5, growth_mb: 2, per_day_mb: 2 },
      },
    }
    render(
      <AppHealthCards
        read={fresh({ database: { total: 1, connections: {}, status: 'pass', growth: week } })}
      />
    )
    expect(
      screen.getByText(
        'Growth per day: the average over the last 3 days. In the last day all data grew +2.0 MB.'
      )
    ).toBeInTheDocument()
  })

  // "over the last 1 days", and a last day with samples from only part of
  // it called "the last day".
  it('names each growth window by its own span, one day or hour as one', () => {
    const oneDay = {
      ...growth,
      window_hours: 25,
      last_day: {
        window_hours: 23.8,
        since: '2026-10-19T11:00:00+05:30',
        stores: {},
        total: { size_mb: 163.5, growth_mb: 6, per_day_mb: null },
      },
    }
    const { unmount } = render(
      <AppHealthCards
        read={fresh({ database: { total: 1, connections: {}, status: 'pass', growth: oneDay } })}
      />
    )
    expect(screen.getByText('Growth per day: the average over the last day.')).toBeInTheDocument()
    unmount()

    const partOfDay = {
      ...oneDay,
      window_hours: 48,
      last_day: { ...oneDay.last_day, window_hours: 12 },
    }
    render(
      <AppHealthCards
        read={fresh({ database: { total: 1, connections: {}, status: 'pass', growth: partOfDay } })}
      />
    )
    expect(
      screen.getByText(
        'Growth per day: the average over the last 2 days. In the last 12 hours all data grew +6.0 MB.'
      )
    ).toBeInTheDocument()
  })

  it('says growth over the last hour, not 1 hours', () => {
    const anHour = {
      window_hours: 1.2,
      since: '2026-10-20T09:00:00+05:30',
      stores: {},
      total: { size_mb: 40, growth_mb: 0.1, per_day_mb: null },
    }
    render(
      <AppHealthCards
        read={fresh({ database: { total: 1, connections: {}, status: 'pass', growth: anHour } })}
      />
    )
    expect(screen.getByText(/^Growth over the last hour;/)).toBeInTheDocument()
  })

  it('shows the growth itself, not a daily rate, over less than a day of samples', () => {
    const hours = {
      window_hours: 5,
      since: '2026-10-20T05:00:00+05:30',
      stores: { logs: { size_mb: 40, growth_mb: 0.4, per_day_mb: null } },
      total: { size_mb: 40, growth_mb: 0.4, per_day_mb: null },
    }
    render(
      <AppHealthCards
        read={fresh({ database: { total: 1, connections: {}, status: 'pass', growth: hours } })}
      />
    )
    expect(rowValue('Logs')).toBe('40.0 MB (+0.4 MB in 5 h)')
    expect(screen.queryByText(/\/day/)).not.toBeInTheDocument()
    expect(screen.getByText(/Growth over the last 5 hours/)).toBeInTheDocument()
  })

  it('says what was missed on the market data: prices by stops and orders, apart from apps', () => {
    render(
      <AppHealthCards
        read={fresh({
          market_data: {
            status: 'warn',
            broker_feeds: [
              feed(
                'market_data',
                { strategy: 3, feed_clients: 2 },
                { strategy: 3, scalping: 0, feed_clients: 2 }
              ),
            ],
            feed_clients: { coalesced: 10, dropped: 1, skipped: 0 },
          },
        })}
      />
    )
    expect(rowValue('Broker market data')).toBe('1,200 updates since it connected')
    expect(rowValue('Prices missed by stops and orders')).toBe('3: strategies 3')
    expect(rowValue('Missed by apps on the feed')).toBe('3 updates')
    expect(rowValue('Replaced by a newer price')).toBe('10')
    const warning = screen.getByRole('status')
    expect(warning).toHaveTextContent(/Strategies, scalping stops or sandbox orders skipped/)
    expect(warning).toHaveTextContent(/Apps on the market data feed missed/)
  })

  // PROD-REL-05: the card counted only since the feed connected, so after a
  // reconnect every price a stop had missed was gone from the page.
  it('still shows the prices a stop missed after the feed reconnected', () => {
    render(
      <AppHealthCards
        read={fresh({
          market_data: {
            status: 'warn',
            broker_feeds: [feed('market_data', {}, { scalping: 7, sandbox: 1 })],
            feed_clients: { coalesced: 0, dropped: 0, skipped: 0 },
          },
        })}
      />
    )
    expect(rowValue('Prices missed by stops and orders')).toBe(
      '8: scalping stops 7, sandbox orders 1'
    )
    expect(screen.getByRole('status')).toHaveTextContent(/skipped some prices/)
    expect(screen.getByText(/counts are since OpenAlgo started/)).toBeInTheDocument()
  })

  // PROD-REL-05: what the bridge skipped relaying the deeper book to apps
  // is counted against the depth socket, which health did not list.
  it('names the market depth feed and counts what apps missed on it', () => {
    render(
      <AppHealthCards
        read={fresh({
          market_data: {
            status: 'warn',
            broker_feeds: [
              feed('market_data', {}, {}),
              feed('market_depth', { feed_clients: 4 }, { feed_clients: 4 }),
            ],
            feed_clients: { coalesced: 0, dropped: 0, skipped: 0 },
          },
        })}
      />
    )
    expect(rowValue('Broker market depth')).toBe('1,200 updates since it connected')
    expect(rowValue('Missed by apps on the feed')).toBe('4 updates')
    expect(screen.getByRole('status')).toHaveTextContent(/Apps on the market data feed missed/)
  })

  it('reports a slow app getting newer prices on its own, never as a loss', () => {
    render(
      <AppHealthCards
        read={fresh({
          market_data: {
            status: 'pass',
            broker_feeds: [feed('market_data', {}, {})],
            feed_clients: { coalesced: 500, dropped: 0, skipped: 0 },
          },
        })}
      />
    )
    expect(rowValue('Replaced by a newer price')).toBe('500')
    expect(rowValue('Missed by apps on the feed')).toBe('none')
    expect(rowValue('Prices missed by stops and orders')).toBe('none')
    expect(screen.getByText(/a replaced update is not a missed one/)).toBeInTheDocument()
    expect(screen.queryByRole('status')).not.toBeInTheDocument()
  })

  it("says where order updates come from and warns when the broker's order feed is down or a subscription was refused", () => {
    render(
      <AppHealthCards
        read={fresh({
          market_data: {
            status: 'warn',
            broker_feeds: [
              {
                ...feed('market_data', {}, {}),
                connections: {
                  per_connection: 1000,
                  most: 3,
                  open: [
                    { instruments: 1000, state: 'connected' },
                    { instruments: 200, state: 'connected' },
                  ],
                },
              },
            ],
            feed_clients: { coalesced: 0, dropped: 0, skipped: 0 },
            subscriptions: {
              waiting: 2,
              releasing: 0,
              release_retrying: 1,
              refused: 1,
              refused_items: [
                {
                  symbol: 'INFY',
                  exchange: 'NSE',
                  mode: 'Quote',
                  reason:
                    'You can stream at most 3000 instruments at once. Unsubscribe some before adding more.',
                },
              ],
            },
            order_updates: { source: 'order_socket', connected: false },
          },
        })}
      />
    )
    expect(rowValue('Order updates')).toBe("Broker's order feed not connected")
    expect(rowValue('Broker market data')).toBe(
      '1,200 updates since it connected, 1,200 symbols on 2 connections'
    )
    expect(rowValue('Subscriptions in progress')).toBe(
      '2 waiting to stream, 1 the broker still streams, trying again'
    )
    const warning = screen.getByRole('status')
    expect(warning).toHaveTextContent(
      /The broker did not take 1 subscription: You can stream at most 3000 instruments/
    )
    expect(warning).toHaveTextContent(/Order updates are not arriving from the broker now/)
  })

  it('counts the symbols a further connection that is not up does not stream', () => {
    render(
      <AppHealthCards
        read={fresh({
          market_data: {
            status: 'warn',
            broker_feeds: [
              {
                ...feed('market_data', {}, {}),
                connections: {
                  per_connection: 1000,
                  most: 3,
                  open: [
                    { instruments: 1000, state: 'connected' },
                    { instruments: 1, state: 'auth_failed' },
                  ],
                },
              },
            ],
            feed_clients: { coalesced: 0, dropped: 0, skipped: 0 },
          },
        })}
      />
    )
    expect(rowValue('Broker market data')).toBe(
      '1,200 updates since it connected, 1,001 symbols on 2 connections; 1 of them not streaming'
    )
  })

  it('names an order source that is up without warning', () => {
    render(
      <AppHealthCards
        read={fresh({
          market_data: {
            status: 'pass',
            broker_feeds: [feed('market_data', {}, {})],
            feed_clients: { coalesced: 0, dropped: 0, skipped: 0 },
            order_updates: { source: 'broker_adapter', connected: null },
          },
        })}
      />
    )
    expect(rowValue('Order updates')).toBe("Read from the broker's order book")
    expect(rowValue('Broker market data')).toBe('1,200 updates since it connected')
    expect(screen.queryByRole('status')).not.toBeInTheDocument()
  })

  it('stays quiet when nothing was missed or lost', () => {
    render(
      <AppHealthCards
        read={fresh({
          market_data: {
            status: 'pass',
            broker_feeds: [],
            feed_clients: { coalesced: 0, dropped: 0, skipped: 0 },
          },
          records: {
            status: 'pass',
            order_logs_dropped: 0,
            order_logs_failed: 0,
            monitoring_dropped: 0,
            monitoring_lost: 0,
            monitoring_retried_batches: 0,
          },
          strategy_ticks: {
            status: 'pass',
            ticks_queued: 50,
            ticks_folded: 0,
            ticks_missed: 0,
            exits_waiting: 0,
            oldest_exit_wait_s: null,
            exits_slow: false,
          },
        })}
      />
    )
    expect(screen.queryByRole('status')).not.toBeInTheDocument()
    expect(screen.getAllByText('all saved')).toHaveLength(2)
    expect(screen.getByText('Strategy Prices')).toBeInTheDocument()
    expect(screen.getByText('Strategies with exits waiting')).toBeInTheDocument()
    expect(screen.getByText('Longest exit wait').nextSibling).toHaveTextContent('none')
  })

  it('warns when a strategy exit has waited too long to be placed', () => {
    render(
      <AppHealthCards
        read={fresh({
          strategy_ticks: {
            status: 'warn',
            ticks_queued: 50,
            ticks_folded: 0,
            ticks_missed: 0,
            exits_waiting: 2,
            oldest_exit_wait_s: 20,
            exits_slow: true,
          },
        })}
      />
    )
    expect(screen.getByText('Strategies with exits waiting').nextSibling).toHaveTextContent('2')
    expect(screen.getByText('20 s')).toBeInTheDocument()
    expect(screen.getByRole('status')).toHaveTextContent(
      /strategy exit decided 20 seconds ago is not placed yet.*Check that strategy's positions with your broker/
    )
    expect(screen.getByRole('status')).not.toHaveTextContent(/missed some prices/)
  })

  it('warns when strategy prices were missed and names every order tracking gap', () => {
    render(
      <AppHealthCards
        read={fresh({
          strategy_ticks: {
            status: 'warn',
            ticks_queued: 50,
            ticks_folded: 4,
            ticks_missed: 2,
            exits_waiting: 1,
            oldest_exit_wait_s: 3,
            exits_slow: false,
          },
          order_facts: {
            status: 'fail',
            gaps: [
              { mode: 'live', cause: 'order_feed_reconnected', since: '2026-10-20T04:30:00Z' },
              { mode: 'sandbox', cause: 'event_loss', since: '2026-10-20T04:31:00Z' },
            ],
            running: true,
            passes: 3,
            book_reads: 3,
            book_read_failures: 0,
            book_reads_discarded: 0,
            gaps_opened: 2,
            gaps_closed: 0,
            unconfirmed_orders: 0,
            bus: {
              published: 10,
              dropped: 0,
              critical_dropped: 0,
              closed_sends: 0,
              panics: 0,
              critical_panics: 0,
              stale_dropped: 0,
              relay_lagged: 0,
            },
          },
        })}
      />
    )
    const notes = screen.getAllByRole('status').map((n) => n.textContent)
    expect(notes).toContainEqual(
      expect.stringMatching(/Strategy risk management missed some prices/)
    )
    expect(notes).toContainEqual(
      expect.stringMatching(
        /Live since 20 Oct, 10:00:00 am IST: the broker's order feed reconnected; Sandbox since 20 Oct, 10:01:00 am IST: an order update could not be delivered inside OpenAlgo.*re-checking the broker's order book/
      )
    )
  })

  // A gap open since yesterday read "Live since 3:20:05 pm IST".
  it('names the date of a gap that opened before today, and only the time of one today', () => {
    const facts = (since: string) => ({
      status: 'fail' as const,
      gaps: [{ mode: 'live', cause: 'event_loss', since }],
      running: true,
      passes: 1,
      book_reads: 1,
      book_read_failures: 0,
      book_reads_discarded: 0,
      gaps_opened: 1,
      gaps_closed: 0,
      unconfirmed_orders: 0,
    })
    const yesterday = new Date(Date.now() - 86_400_000).toISOString()
    const { unmount } = render(<AppHealthCards read={fresh({ order_facts: facts(yesterday) })} />)
    expect(screen.getByRole('status')).toHaveTextContent(
      /Live since \d{1,2} [A-Z][a-z]{2}, \d{2}:\d{2}:\d{2} [ap]m IST:/
    )
    unmount()
    render(<AppHealthCards read={fresh({ order_facts: facts(new Date().toISOString()) })} />)
    expect(screen.getByRole('status')).toHaveTextContent(/Live since \d{2}:\d{2}:\d{2} [ap]m IST:/)
  })

  // PROD-PAR-01: with health unread the checks were left out, so a page
  // that could read nothing looked like one with nothing to report.
  it('shows the trader checks as not known when health could not be read', () => {
    render(<AppHealthCards read={{ data: null, readAt: null, failedAt: Date.now() }} />)
    for (const title of ['Market Data', 'Order Tracking', 'Records Saved', 'Strategy Prices']) {
      expect(within(card(title)).getByText('Not known')).toBeInTheDocument()
    }
    expect(screen.getAllByText(/could not read this check/)).toHaveLength(4)
  })

  it('keeps the figures it could not refresh, marked with when they were read', () => {
    const read = fresh({
      records: {
        status: 'warn',
        order_logs_dropped: 2,
        order_logs_failed: 0,
        monitoring_dropped: 0,
        monitoring_lost: 0,
        monitoring_retried_batches: 0,
      },
    })
    render(<AppHealthCards read={{ ...read, failedAt: Date.now() }} />)
    const records = card('Records Saved')
    expect(within(records).getByText('2 not saved')).toBeInTheDocument()
    expect(
      within(records).getByText(/^As read at .* IST; the refresh at .* IST failed\.$/)
    ).toBeInTheDocument()
  })

  // Every card at once, warnings, stale marks, a check not known and the
  // growth note included: rows only inside each description list (a
  // wrapper around a feed's rows, or a note inside the list, is a serious
  // axe finding).
  it('has no accessibility violations with every card, warning and mark shown', async () => {
    const { container } = render(
      <AppHealthCards
        read={{
          data: sample({
            database: {
              total: 1,
              connections: {},
              status: 'pass',
              growth: { ...growth, window_hours: 0.5 },
            },
            market_data: {
              status: 'warn',
              broker_feeds: [
                feed('market_data', { strategy: 4 }, { strategy: 4, feed_clients: 1 }),
                feed('order_updates', {}, { order_relay: 1 }),
              ],
              feed_clients: { coalesced: 3, dropped: 2, skipped: 0 },
            },
            records: {
              status: 'warn',
              order_logs_dropped: 1,
              order_logs_failed: 0,
              monitoring_dropped: 0,
              monitoring_lost: 0,
              monitoring_retried_batches: 0,
            },
            strategy_ticks: {
              status: 'warn',
              ticks_queued: 10,
              ticks_folded: 0,
              ticks_missed: 1,
              exits_waiting: 1,
              oldest_exit_wait_s: 20,
              exits_slow: true,
            },
          }),
          readAt: Date.now() - 10_000,
          failedAt: Date.now(),
        }}
      />
    )
    expect(screen.getByText(/Growth shows once an hour/)).toBeInTheDocument()
    expect(rowValue('Order updates skipped')).toBe('1')
    expect(screen.getByText('Not known')).toBeInTheDocument()
    await checkA11y(container)
  })
})
