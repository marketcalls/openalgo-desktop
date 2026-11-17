/**
 * Bulk Add keeps the symbols it could not add, with each reason
 * (marketcalls/openalgo#2463, web 6716dd6f2). The page reported only how
 * many were added, so a pasted Nifty 500 list that came back 495 of 500 said
 * nothing about the five, and the dialog closed with them.
 */

import { beforeEach, describe, expect, it, vi } from 'vitest'
import { fireEvent, render, screen, waitFor } from '@/test/test-utils'

const toast = vi.hoisted(() => ({
  success: vi.fn(),
  error: vi.fn(),
  warning: vi.fn(),
  info: vi.fn(),
}))

vi.mock('@/utils/toast', () => ({ showToast: toast }))

vi.mock('@/components/socket/SocketProvider', () => ({
  useSocketContext: () => ({ socket: null }),
}))

vi.mock('@/hooks/useProfileMenuItems', () => ({
  useProfileMenuItems: () => [],
}))

import Historify from './Historify'

type BulkAnswer = Record<string, unknown>

/** The server: every page load answers as an empty store, bulk add as given. */
function serve(bulk: BulkAnswer) {
  const sent: unknown[] = []
  const fetchMock = vi.fn(async (url: string, init?: RequestInit) => {
    const json = (body: unknown) => new Response(JSON.stringify(body), { status: 200 })
    if (url === '/auth/csrf-token') return json({ csrf_token: 'token' })
    if (url === '/historify/api/watchlist/bulk') {
      sent.push(JSON.parse(String(init?.body)))
      return json(bulk)
    }
    if (url === '/historify/api/watchlist') return json({ status: 'success', data: [] })
    return json({ status: 'error', message: 'not used here' })
  })
  vi.stubGlobal('fetch', fetchMock)
  return { fetchMock, sent }
}

async function openBulkAdd() {
  render(<Historify />)
  const [bulk] = await screen.findAllByRole('button', { name: /^Bulk$/ })
  fireEvent.click(bulk)
  return screen.findByPlaceholderText(/RELIANCE,NSE/)
}

async function addSymbols(text: string) {
  const box = await openBulkAdd()
  fireEvent.change(box, { target: { value: text } })
  fireEvent.click(screen.getByRole('button', { name: 'Add Symbols' }))
}

describe('Historify Bulk Add', () => {
  beforeEach(() => {
    vi.unstubAllGlobals()
    for (const fn of Object.values(toast)) fn.mockReset()
  })

  it('keeps the symbols that were not added, each with its reason', async () => {
    const { sent } = serve({
      status: 'success',
      added: 2,
      skipped: 1,
      failed: [
        {
          symbol: 'A_B',
          exchange: 'NSE',
          error: "Symbol 'A_B' could be A-B or A&B on NSE. Enter the exact symbol.",
        },
        {
          symbol: 'HFCL-BE',
          exchange: 'NSE',
          error: "Symbol 'HFCL-BE' not found in NSE master contract. Please check the symbol name.",
        },
      ],
      renamed: [{ from: 'M_M', to: 'M&M', exchange: 'NSE' }],
      total: 5,
    })

    await addSymbols('RELIANCE,NSE\nM_M,NSE\nM&M,NSE\nA_B,NSE\nHFCL-BE,NSE')

    await waitFor(() => expect(toast.warning).toHaveBeenCalled())
    expect(sent).toEqual([
      {
        symbols: [
          { symbol: 'RELIANCE', exchange: 'NSE' },
          { symbol: 'M_M', exchange: 'NSE' },
          { symbol: 'M&M', exchange: 'NSE' },
          { symbol: 'A_B', exchange: 'NSE' },
          { symbol: 'HFCL-BE', exchange: 'NSE' },
        ],
      },
    ])
    expect(toast.warning).toHaveBeenCalledWith(
      'Added 2 symbols, 1 already in watchlist. 2 not added.',
      'historify',
      { description: 'Added as: M_M as M&M' }
    )
    expect(toast.success).not.toHaveBeenCalled()

    // The dialog stays open with only the refused symbols, ready to correct.
    const box = screen.getByPlaceholderText(/RELIANCE,NSE/) as HTMLTextAreaElement
    expect(box.value).toBe('A_B,NSE\nHFCL-BE,NSE')
    expect(
      screen.getByText('2 symbols were not added. Correct them above and add again.')
    ).toBeInTheDocument()
    expect(screen.getByText(/could be A-B or A&B on NSE/)).toBeInTheDocument()
    expect(screen.getByText(/'HFCL-BE' not found in NSE master contract/)).toBeInTheDocument()

    // Cancel forgets the refusals: the next open starts clean.
    fireEvent.click(screen.getByRole('button', { name: 'Cancel' }))
    await waitFor(() => expect(screen.queryByPlaceholderText(/RELIANCE,NSE/)).toBeNull())
    const [bulk] = screen.getAllByRole('button', { name: /^Bulk$/ })
    fireEvent.click(bulk)
    await screen.findByPlaceholderText(/RELIANCE,NSE/)
    expect(screen.queryByText(/were not added/)).toBeNull()
  })

  it('closes when every symbol was added and names the ones renamed', async () => {
    serve({
      status: 'success',
      added: 1,
      skipped: 0,
      failed: [],
      renamed: [{ from: 'BAJAJ_AUTO', to: 'BAJAJ-AUTO', exchange: 'NSE' }],
      total: 1,
    })

    await addSymbols('BAJAJ_AUTO,NSE')

    await waitFor(() =>
      expect(toast.success).toHaveBeenCalledWith('Added 1 symbol', 'historify', {
        description: 'Added as: BAJAJ_AUTO as BAJAJ-AUTO',
      })
    )
    await waitFor(() => expect(screen.queryByPlaceholderText(/RELIANCE,NSE/)).toBeNull())
    expect(toast.warning).not.toHaveBeenCalled()
  })

  it('reads an answer without renamed, as an older server sends it', async () => {
    serve({ status: 'success', added: 2, skipped: 0, failed: [], total: 2 })

    await addSymbols('RELIANCE,NSE\nSBIN,NSE')

    await waitFor(() =>
      expect(toast.success).toHaveBeenCalledWith('Added 2 symbols', 'historify', {
        description: undefined,
      })
    )
  })
})
