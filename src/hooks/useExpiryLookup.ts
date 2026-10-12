import { useCallback, useEffect, useRef, useState } from 'react'
import { refusalSentence } from '@/lib/serverSentence'

/**
 * The expiry lookup every option tool runs when its underlying changes
 * (`GET /search/api/expiries?...&instrumenttype=options`), with an explicit
 * outcome for each way it can end.
 *
 * Desktop: the web pages treat any outcome other than a non-empty list as
 * "no expiries" (or a passing toast), leave the Select Expiry dropdown
 * empty and the tool waiting, and offer no way to try again. A trader then
 * sees an empty dropdown and a spinner that never stops. Each page now
 * shows what happened and a Retry (TOOLS-02); the request and the
 * selection of the first live date are unchanged.
 */

export type ExpiryLookupPhase =
  /** No underlying selected yet. */
  | 'idle'
  | 'loading'
  /** Live dates arrived; the first is selected. */
  | 'ready'
  /** The lookup worked and the underlying has no live expiry. */
  | 'empty'
  /** No master contract is loaded, so nothing can be listed yet. */
  | 'master'
  /** The session was not accepted. */
  | 'signin'
  /** The lookup failed, timed out or could not reach OpenAlgo. */
  | 'failed'

export interface ExpiryLookup {
  phase: ExpiryLookupPhase
  expiries: string[]
  /** The sentence for the trader: empty while idle, loading or ready. */
  message: string
  /** Run the lookup again for the same underlying. */
  retry: () => void
}

/** The answer of the expiry lookup, as the pages' API modules return it. */
export interface ExpiriesAnswer {
  status: 'success' | 'error'
  expiries?: string[]
  message?: string
  master_ready?: boolean
}

export type FetchExpiries = (exchange: string, underlying: string) => Promise<ExpiriesAnswer>

export interface ExpiryLookupOptions {
  /** Runs as a lookup starts: clear the previous underlying's dates and data. */
  onReset?: () => void
  /** Runs with the live dates, earliest first, when they arrive. */
  onReady?: (expiries: string[]) => void
  /** How long to wait before calling the lookup failed. */
  timeoutMs?: number
}

export const EXPIRY_LOOKUP_TIMEOUT_MS = 15_000

export const EXPIRY_TEXT = {
  master:
    'Expiry dates are not available until the master contract is loaded. Open Master Contract to download it, then retry.',
  signin: 'Your session could not be confirmed. Sign in again, then retry.',
  timeout: 'Loading expiry dates took too long. Check that OpenAlgo is running, then retry.',
  unreachable: 'Could not reach OpenAlgo to load expiry dates. Retry in a moment.',
  failed: 'Could not load expiry dates. Retry in a moment.',
  empty: (underlying: string, exchange: string) =>
    `No live expiry dates are listed for ${underlying} on ${exchange}. Choose another underlying, or download the master contract again if you expected some.`,
} as const

class LookupTimeout extends Error {
  constructor() {
    super('expiry lookup timed out')
    this.name = 'LookupTimeout'
  }
}

interface Outcome {
  phase: Exclude<ExpiryLookupPhase, 'idle' | 'loading'>
  expiries: string[]
  message: string
}

function statusOf(error: unknown): number | undefined {
  const response = (error as { response?: { status?: unknown } } | null)?.response
  return typeof response?.status === 'number' ? response.status : undefined
}

function dataOf(error: unknown): unknown {
  return (error as { response?: { data?: unknown } } | null)?.response?.data
}

/** What a finished lookup means for the page. */
export function expiryOutcome(
  result: { answer: ExpiriesAnswer } | { error: unknown },
  underlying: string,
  exchange: string
): Outcome {
  if ('answer' in result) {
    const answer = result.answer
    const expiries = Array.isArray(answer?.expiries) ? answer.expiries : []
    if (answer?.master_ready === false) {
      return { phase: 'master', expiries: [], message: answer.message || EXPIRY_TEXT.master }
    }
    if (answer?.status === 'success') {
      return expiries.length > 0
        ? { phase: 'ready', expiries, message: '' }
        : { phase: 'empty', expiries: [], message: EXPIRY_TEXT.empty(underlying, exchange) }
    }
    return {
      phase: 'failed',
      expiries: [],
      message: (typeof answer?.message === 'string' && answer.message.trim()) || EXPIRY_TEXT.failed,
    }
  }
  const error = result.error
  if (error instanceof LookupTimeout) {
    return { phase: 'failed', expiries: [], message: EXPIRY_TEXT.timeout }
  }
  const status = statusOf(error)
  if (status === 401 || status === 403) {
    return { phase: 'signin', expiries: [], message: EXPIRY_TEXT.signin }
  }
  if (status === undefined) {
    return { phase: 'failed', expiries: [], message: EXPIRY_TEXT.unreachable }
  }
  return {
    phase: 'failed',
    expiries: [],
    message: refusalSentence(status, dataOf(error)) ?? EXPIRY_TEXT.failed,
  }
}

/**
 * Look up the live expiries whenever `exchange` or `underlying` changes, or
 * the trader asks to retry. Only the newest lookup can change the page: an
 * answer for an underlying the trader has since left is dropped.
 */
export function useExpiryLookup(
  exchange: string,
  underlying: string,
  fetchExpiries: FetchExpiries,
  options: ExpiryLookupOptions = {}
): ExpiryLookup {
  const [state, setState] = useState<Omit<ExpiryLookup, 'retry'>>({
    phase: 'idle',
    expiries: [],
    message: '',
  })
  const [attempt, setAttempt] = useState(0)
  // The latest callbacks, so a page passing inline functions does not
  // restart the lookup on every render.
  const fetchRef = useRef(fetchExpiries)
  const optionsRef = useRef(options)
  fetchRef.current = fetchExpiries
  optionsRef.current = options

  // biome-ignore lint/correctness/useExhaustiveDependencies: `attempt` is the retry trigger; the callbacks are read through refs on purpose
  useEffect(() => {
    if (!underlying) {
      setState({ phase: 'idle', expiries: [], message: '' })
      return
    }
    optionsRef.current.onReset?.()
    setState({ phase: 'loading', expiries: [], message: '' })

    let cancelled = false
    let timer: ReturnType<typeof setTimeout> | undefined
    const timeout = new Promise<never>((_, reject) => {
      timer = setTimeout(
        () => reject(new LookupTimeout()),
        optionsRef.current.timeoutMs ?? EXPIRY_LOOKUP_TIMEOUT_MS
      )
    })
    const run = async () => {
      let result: { answer: ExpiriesAnswer } | { error: unknown }
      try {
        result = { answer: await Promise.race([fetchRef.current(exchange, underlying), timeout]) }
      } catch (error) {
        result = { error }
      } finally {
        clearTimeout(timer)
      }
      if (cancelled) return
      const outcome = expiryOutcome(result, underlying, exchange)
      setState(outcome)
      if (outcome.phase === 'ready') optionsRef.current.onReady?.(outcome.expiries)
    }
    run()
    return () => {
      cancelled = true
      clearTimeout(timer)
    }
  }, [exchange, underlying, attempt])

  const retry = useCallback(() => setAttempt((n) => n + 1), [])

  return { ...state, retry }
}
