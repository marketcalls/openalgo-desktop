import { act, renderHook, waitFor } from '@testing-library/react'
import axios from 'axios'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { axiosFailure } from '@/test/axiosAnswer'
import {
  EXPIRY_LOOKUP_TIMEOUT_MS,
  EXPIRY_TEXT,
  type ExpiriesAnswer,
  expiryOutcome,
  useExpiryLookup,
} from './useExpiryLookup'

/** A real axios failure with the status and body the server sent. */
const failure = (status: number, data: unknown) =>
  axiosFailure((adapter) => axios.get('/search/api/expiries', { adapter }), status, data)

describe('expiryOutcome', () => {
  const of = (answer: ExpiriesAnswer) => expiryOutcome({ answer }, 'NIFTY', 'NFO')
  const thrown = (error: unknown) => expiryOutcome({ error }, 'NIFTY', 'NFO')

  it('tells dates, an empty calendar and a missing master apart', () => {
    expect(of({ status: 'success', expiries: ['13-OCT-26'] })).toEqual({
      phase: 'ready',
      expiries: ['13-OCT-26'],
      message: '',
    })
    expect(of({ status: 'success', expiries: [] })).toEqual({
      phase: 'empty',
      expiries: [],
      message: EXPIRY_TEXT.empty('NIFTY', 'NFO'),
    })
    const master = of({
      status: 'error',
      expiries: [],
      master_ready: false,
      message: 'The master is loading.',
    })
    expect(master.phase).toBe('master')
    expect(master.message).toBe('The master is loading.')
  })

  it('keeps the server sentence of a JSON error answered with 200', () => {
    expect(of({ status: 'error', message: 'Expiry lookup is not available.' })).toEqual({
      phase: 'failed',
      expiries: [],
      message: 'Expiry lookup is not available.',
    })
    expect(of({ status: 'error' }).message).toBe(EXPIRY_TEXT.failed)
  })

  it('names refusals, busy answers and an unreachable server', async () => {
    expect(thrown(await failure(401, {})).phase).toBe('signin')
    expect(thrown(await failure(403, {})).phase).toBe('signin')
    const busy = thrown(
      await failure(429, { status: 'error', message: 'Wait a few seconds and retry.' })
    )
    expect(busy).toEqual({
      phase: 'failed',
      expiries: [],
      message: 'Wait a few seconds and retry.',
    })
    expect(thrown(await failure(500, { message: 'Traceback' })).message).toBe(EXPIRY_TEXT.failed)
    expect(thrown(new Error('Network Error')).message).toBe(EXPIRY_TEXT.unreachable)
  })
})

describe('useExpiryLookup', () => {
  afterEach(() => {
    vi.useRealTimers()
  })

  it('reports loading, then the dates, and selects through onReady', async () => {
    const onReset = vi.fn()
    const onReady = vi.fn()
    const fetchExpiries = vi
      .fn()
      .mockResolvedValue({ status: 'success', expiries: ['13-OCT-26', '20-OCT-26'] })
    const { result } = renderHook(() =>
      useExpiryLookup('NFO', 'NIFTY', fetchExpiries, { onReset, onReady })
    )
    expect(result.current.phase).toBe('loading')
    expect(onReset).toHaveBeenCalledTimes(1)
    await waitFor(() => expect(result.current.phase).toBe('ready'))
    expect(onReady).toHaveBeenCalledWith(['13-OCT-26', '20-OCT-26'])
    expect(fetchExpiries).toHaveBeenCalledWith('NFO', 'NIFTY')
  })

  it('stays idle with no underlying', () => {
    const fetchExpiries = vi.fn()
    const { result } = renderHook(() => useExpiryLookup('NFO', '', fetchExpiries))
    expect(result.current.phase).toBe('idle')
    expect(fetchExpiries).not.toHaveBeenCalled()
  })

  it('retries the same lookup and recovers', async () => {
    const onReady = vi.fn()
    const fetchExpiries = vi
      .fn()
      .mockRejectedValueOnce(new Error('Network Error'))
      .mockResolvedValueOnce({ status: 'success', expiries: ['13-OCT-26'] })
    const { result } = renderHook(() => useExpiryLookup('NFO', 'NIFTY', fetchExpiries, { onReady }))
    await waitFor(() => expect(result.current.phase).toBe('failed'))
    expect(result.current.message).toBe(EXPIRY_TEXT.unreachable)
    act(() => result.current.retry())
    await waitFor(() => expect(result.current.phase).toBe('ready'))
    expect(fetchExpiries).toHaveBeenCalledTimes(2)
    expect(onReady).toHaveBeenCalledWith(['13-OCT-26'])
  })

  it('gives up on a lookup that never answers', async () => {
    vi.useFakeTimers()
    const fetchExpiries = vi.fn(() => new Promise<ExpiriesAnswer>(() => {}))
    const { result } = renderHook(() => useExpiryLookup('NFO', 'NIFTY', fetchExpiries))
    expect(result.current.phase).toBe('loading')
    await act(async () => {
      await vi.advanceTimersByTimeAsync(EXPIRY_LOOKUP_TIMEOUT_MS)
    })
    expect(result.current.phase).toBe('failed')
    expect(result.current.message).toBe(EXPIRY_TEXT.timeout)
  })

  it('drops the answer for an underlying the trader has left', async () => {
    let answerNifty: (a: ExpiriesAnswer) => void = () => {}
    const onReady = vi.fn()
    const fetchExpiries = vi.fn((_exchange: string, underlying: string) =>
      underlying === 'NIFTY'
        ? new Promise<ExpiriesAnswer>((resolve) => {
            answerNifty = resolve
          })
        : Promise.resolve<ExpiriesAnswer>({ status: 'success', expiries: ['28-OCT-26'] })
    )
    const { result, rerender } = renderHook(
      ({ u }) => useExpiryLookup('NFO', u, fetchExpiries, { onReady }),
      { initialProps: { u: 'NIFTY' } }
    )
    rerender({ u: 'BANKNIFTY' })
    await waitFor(() => expect(result.current.phase).toBe('ready'))
    await act(async () => {
      answerNifty({ status: 'success', expiries: ['13-OCT-26'] })
    })
    expect(result.current.expiries).toEqual(['28-OCT-26'])
    expect(onReady).toHaveBeenCalledTimes(1)
    expect(onReady).toHaveBeenCalledWith(['28-OCT-26'])
  })
})
