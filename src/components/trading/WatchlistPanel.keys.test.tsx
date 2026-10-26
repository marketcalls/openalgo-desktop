import { beforeEach, describe, expect, it, vi } from 'vitest'
import type { Watchlist } from '@/api/watchlist'
import { fireEvent, render, screen } from '@/test/test-utils'
import { WatchlistPanel } from './WatchlistPanel'

/**
 * Issue #198: Up and Down walk the /trading watchlist and chart the row they
 * land on. The API and the price hook are mocked as in WatchlistPanel.test.tsx;
 * what is under test is which row is focused and charted for each key.
 */
const list = vi.fn()
vi.mock('@/api/watchlist', () => ({
  watchlistApi: {
    list: (...a: unknown[]) => list(...a),
    create: vi.fn(),
    rename: vi.fn(),
    remove: vi.fn(),
    clear: vi.fn(),
    addItem: vi.fn(),
    removeItem: vi.fn(),
    reorderItems: vi.fn(),
  },
  watchlistError: (_e: unknown, fallback: string) => fallback,
}))

vi.mock('@/hooks/useLivePrice', () => ({
  useLivePrice: () => ({
    data: [],
    multiQuotes: new Map(),
    isLive: true,
    isFallbackMode: false,
    isAnyMarketOpen: true,
  }),
}))

vi.mock('@/lib/trading/previousClose', async (importOriginal) => ({
  ...(await importOriginal<object>()),
  previousClose: async () => null,
}))

const LIST: Watchlist = {
  id: 1,
  name: 'Banks',
  position: 0,
  items: [
    { id: 10, symbol: 'SBIN', exchange: 'NSE', position: 0 },
    { id: 11, symbol: 'HDFCBANK', exchange: 'NSE', position: 1 },
    { id: 12, symbol: 'ICICIBANK', exchange: 'NSE', position: 2 },
  ],
}

const row = (symbol: string) => screen.getByRole('button', { name: `Chart ${symbol} on NSE` })

async function renderPanel() {
  const onPick = vi.fn()
  render(<WatchlistPanel apiKey="k" onPick={onPick} search={async () => []} />)
  await screen.findByRole('button', { name: 'Chart SBIN on NSE' })
  return onPick
}

describe('watchlist arrow keys', () => {
  beforeEach(() => {
    localStorage.clear()
    list.mockReset()
    list.mockResolvedValue([LIST])
  })

  it('moves down and up from the clicked row, charting each row it lands on', async () => {
    const onPick = await renderPanel()
    // A plain click event: WebKit, the macOS window, does not focus a button
    // on click, so the panel does.
    fireEvent.click(row('SBIN'))
    expect(row('SBIN')).toHaveFocus()
    expect(onPick).toHaveBeenLastCalledWith({ symbol: 'SBIN', exchange: 'NSE' })

    fireEvent.keyDown(row('SBIN'), { key: 'ArrowDown' })
    expect(row('HDFCBANK')).toHaveFocus()
    expect(onPick).toHaveBeenLastCalledWith({ symbol: 'HDFCBANK', exchange: 'NSE' })

    fireEvent.keyDown(row('HDFCBANK'), { key: 'ArrowDown' })
    fireEvent.keyDown(row('ICICIBANK'), { key: 'ArrowUp' })
    expect(row('HDFCBANK')).toHaveFocus()
    expect(onPick.mock.calls.map((c) => c[0].symbol)).toEqual([
      'SBIN',
      'HDFCBANK',
      'ICICIBANK',
      'HDFCBANK',
    ])
  })

  it('stays on the first and last rows, and keeps the key from the chart', async () => {
    const onPick = await renderPanel()
    const chart = vi.fn()
    window.addEventListener('keydown', chart)
    try {
      row('ICICIBANK').focus()
      const kept = !fireEvent.keyDown(row('ICICIBANK'), { key: 'ArrowDown' })
      expect(kept).toBe(true)
      expect(row('ICICIBANK')).toHaveFocus()
      row('SBIN').focus()
      fireEvent.keyDown(row('SBIN'), { key: 'ArrowUp' })
      expect(row('SBIN')).toHaveFocus()
      expect(onPick).not.toHaveBeenCalled()
      fireEvent.keyDown(row('SBIN'), { key: 'ArrowDown' })
      expect(chart).not.toHaveBeenCalled()
    } finally {
      window.removeEventListener('keydown', chart)
    }
  })

  it('leaves arrows with a modifier, and other keys, alone', async () => {
    const onPick = await renderPanel()
    row('SBIN').focus()
    expect(fireEvent.keyDown(row('SBIN'), { key: 'ArrowDown', shiftKey: true })).toBe(true)
    fireEvent.keyDown(row('SBIN'), { key: 'ArrowDown', metaKey: true })
    fireEvent.keyDown(row('SBIN'), { key: 'ArrowRight' })
    expect(row('SBIN')).toHaveFocus()
    expect(onPick).not.toHaveBeenCalled()
  })

  it('charts the row a held key is let go on, not every row on the way', async () => {
    const onPick = await renderPanel()
    row('SBIN').focus()
    fireEvent.keyDown(row('SBIN'), { key: 'ArrowDown' })
    fireEvent.keyDown(row('HDFCBANK'), { key: 'ArrowDown', repeat: true })
    expect(row('ICICIBANK')).toHaveFocus()
    expect(onPick.mock.calls.map((c) => c[0].symbol)).toEqual(['HDFCBANK'])
    fireEvent.keyUp(row('ICICIBANK'), { key: 'ArrowDown' })
    expect(onPick.mock.calls.map((c) => c[0].symbol)).toEqual(['HDFCBANK', 'ICICIBANK'])
  })

  it('tells a screen reader which keys the rows take', async () => {
    await renderPanel()
    expect(row('SBIN')).toHaveAttribute('aria-keyshortcuts', 'ArrowUp ArrowDown')
  })
})
