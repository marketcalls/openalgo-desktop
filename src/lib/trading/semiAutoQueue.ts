/**
 * Desktop: a chart order queued for approval (docs-43).
 *
 * With the API key's order mode on Semi-Auto, `/api/v1/placeorder` does not
 * send the order: it queues it in Action Center and answers
 * `{status: 'success', mode: 'semi_auto', pending_order_id, message}`, with
 * no `orderid` (services/order_router.rs). The chart's trade feed reads a
 * missing order id as a failure and threw "placeorder returned no orderid",
 * so the trader read an error for an order that was waiting for them. The
 * web has the same defect.
 *
 * The fetch below, handed to the feed, turns that answer into
 * `QueuedForApproval`, which the terminal reports as what it is. Nothing is
 * sent twice: the order is already queued when the answer arrives.
 */

/** What the trader reads once a chart order is queued. */
export const WAITING_FOR_APPROVAL =
  'Order waiting for your approval in Action Center. It goes to your broker once you approve it there.'

/** A placeorder the server queued in Action Center instead of sending it. */
export class QueuedForApproval extends Error {
  constructor() {
    super(WAITING_FOR_APPROVAL)
    this.name = 'QueuedForApproval'
  }
}

export function isQueuedForApproval(e: unknown): e is QueuedForApproval {
  return e instanceof QueuedForApproval
}

function isPlaceOrder(input: RequestInfo | URL): boolean {
  const url = typeof input === 'string' ? input : input instanceof URL ? input.href : input.url
  return /\/api\/v1\/placeorder\/?(\?|$)/.test(url)
}

/** Whether a placeorder answer says the order was queued for approval. */
export function isQueuedAnswer(body: unknown): boolean {
  if (!body || typeof body !== 'object') return false
  const b = body as Record<string, unknown>
  return b.status === 'success' && b.mode === 'semi_auto' && b.orderid == null && b.order_id == null
}

/**
 * The trade feed's fetch: every request goes through unchanged, except that
 * a placeorder answered "queued for approval" throws `QueuedForApproval`.
 * `base` is read at call time, so a test can replace the global fetch.
 */
export function queueAwareFetch(base?: typeof fetch): typeof fetch {
  return async (input, init) => {
    const res = await (base ?? globalThis.fetch)(input, init)
    if (!res.ok || !isPlaceOrder(input)) return res
    const body = await res
      .clone()
      .json()
      .catch(() => null)
    if (isQueuedAnswer(body)) throw new QueuedForApproval()
    return res
  }
}
