import { readFileSync } from 'node:fs'
import { join } from 'node:path'
import { OpenAlgoTradeFeed } from 'openalgo-charts'
import { describe, expect, it, vi } from 'vitest'
import { QueuedForApproval, queueAwareFetch, WAITING_FOR_APPROVAL } from './semiAutoQueue'

/** A server that answers placeorder with `answer` and HTTP `status`. */
function server(answer: unknown, status = 200) {
  return vi.fn(
    async (_input: RequestInfo | URL, _init?: RequestInit) =>
      new Response(JSON.stringify(answer), {
        status,
        headers: { 'Content-Type': 'application/json' },
      })
  )
}

function feed(fetchImpl: typeof fetch) {
  return new OpenAlgoTradeFeed({ baseUrl: '', apiKey: 'k', strategy: 'Chart', fetchImpl })
}

const ORDER = {
  symbol: 'SBIN',
  exchange: 'NSE',
  side: 'BUY' as const,
  type: 'MARKET' as const,
  qty: 1,
  product: 'MIS' as const,
}

// The server's answer in Semi-Auto mode (services/order_router.rs).
const QUEUED = {
  status: 'success',
  message: 'Order queued for approval in Action Center',
  mode: 'semi_auto',
  pending_order_id: 7,
}

// docs-43: a Semi-Auto chart order said "placeorder returned no orderid"
// though it was waiting in Action Center.
describe('a chart order queued for approval', () => {
  it('is reported as waiting in Action Center, not as a failure', async () => {
    const post = server(QUEUED)
    const placing = feed(queueAwareFetch(post)).place(ORDER)
    await expect(placing).rejects.toBeInstanceOf(QueuedForApproval)
    await expect(placing).rejects.toThrow(WAITING_FOR_APPROVAL)
    expect(post).toHaveBeenCalledTimes(1)
    expect(String(post.mock.calls[0][0])).toBe('/api/v1/placeorder')
  })

  it('leaves a placed order, a refusal and other requests as they were', async () => {
    const placed = feed(queueAwareFetch(server({ status: 'success', orderid: '2501' })))
    await expect(placed.place(ORDER)).resolves.toEqual({ orderId: '2501' })

    const refused = feed(queueAwareFetch(server({ status: 'error', message: 'Invalid' }, 400)))
    await expect(refused.place(ORDER)).rejects.toThrow(/Invalid/)

    const other = await queueAwareFetch(server(QUEUED))('/api/v1/orderbook', { method: 'POST' })
    expect(await other.json()).toEqual(QUEUED)
  })

  // Read from the source, as terminal.test.ts does: driving a terminal needs
  // a DOM and a broker. Every order the terminal sends through its feed
  // reports the queued answer as such.
  it('is how the terminal places every chart order', () => {
    const src = readFileSync(join(process.cwd(), 'src/lib/trading/terminal.ts'), 'utf8')
    expect(src).toContain('fetchImpl: queueAwareFetch()')
    const places = src.split('this.trade.place(').length - 1
    expect(places).toBe(3)
    expect(src.split('isQueuedForApproval(e)').length - 1).toBe(places)
  })
})
