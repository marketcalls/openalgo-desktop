/**
 * UI-02: a stop shows only once the server has saved it, and a failed save or
 * removal says so. The web showed the stop before the save and dropped the
 * error (`.catch(() => {})`), so a trader could watch a stop that nothing
 * enforced, or see one removed that the server still held.
 */
import { act, renderHook, waitFor } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { type SLState, useTrailingSL } from './useTrailingSL'

const api = vi.hoisted(() => ({
  getSLStates: vi.fn(),
  saveSL: vi.fn(),
  deleteSL: vi.fn(),
}))
const toast = vi.hoisted(() => ({ error: vi.fn(), success: vi.fn() }))

vi.mock('@/api/scalping', () => ({ scalpingApi: api }))
vi.mock('@/utils/toast', () => ({ showToast: toast }))
vi.mock('./useOrderEventRefresh', () => ({ useOrderEventRefresh: () => {} }))

const stop: SLState = {
  symbol: 'NIFTY28OCT2625000CE',
  exchange: 'NFO',
  product: 'NRML',
  side: 'BUY',
  entry: 100,
  quantity: 75,
  initialSl: 90,
  trailingEnabled: false,
  trailingStep: 0,
  highestPrice: 100,
  lowestPrice: 100,
  currentSl: 90,
  target: 0,
  active: true,
}
const key = 'NFO:NIFTY28OCT2625000CE:NRML'
const savedRow = {
  symbol: stop.symbol,
  exchange: 'NFO',
  product: 'NRML',
  side: 'BUY',
  entry_price: 100,
  quantity: 75,
  initial_sl: 90,
  trailing_enabled: false,
  trailing_step: 0,
  highest_price: 100,
  lowest_price: 100,
  current_sl: 90,
  target: 0,
  is_active: true,
}

async function mounted(initial: unknown[] = []) {
  api.getSLStates.mockResolvedValue({ status: 'success', data: initial })
  const hook = renderHook(() => useTrailingSL('live'))
  await waitFor(() => expect(api.getSLStates).toHaveBeenCalled())
  return hook
}

describe('useTrailingSL shows a stop only once the server acknowledged it', () => {
  beforeEach(() => {
    vi.clearAllMocks()
  })

  it('a save the request never completed shows no stop and says the position is unprotected', async () => {
    const { result } = await mounted()
    api.saveSL.mockRejectedValue(new Error('network'))
    let saved: boolean | undefined
    await act(async () => {
      saved = await result.current.setSL(stop)
    })
    expect(saved).toBe(false)
    expect(result.current.slMap[key]).toBeUndefined()
    expect(toast.error).toHaveBeenCalledWith(
      'Stop-loss not saved. This position is not protected. Try again.',
      undefined,
      { duration: Number.POSITIVE_INFINITY }
    )
  })

  it('a save the server refused shows no stop and says so', async () => {
    const { result } = await mounted()
    api.saveSL.mockResolvedValue({ status: 'error', message: 'Failed to save SL state' })
    let saved: boolean | undefined
    await act(async () => {
      saved = await result.current.setSL(stop)
    })
    expect(saved).toBe(false)
    expect(result.current.slMap[key]).toBeUndefined()
    expect(toast.error).toHaveBeenCalledTimes(1)
  })

  it('a saved stop shows as the server recorded it', async () => {
    const { result } = await mounted()
    api.saveSL.mockResolvedValue({ status: 'success', data: { ...savedRow, current_sl: 91 } })
    let saved: boolean | undefined
    await act(async () => {
      saved = await result.current.setSL(stop)
    })
    expect(saved).toBe(true)
    expect(result.current.slMap[key]?.currentSl).toBe(91)
    expect(toast.error).not.toHaveBeenCalled()
  })

  it('a removal that failed keeps the stop on screen and says it is still active', async () => {
    const { result } = await mounted([savedRow])
    await waitFor(() => expect(result.current.slMap[key]).toBeDefined())
    api.deleteSL.mockRejectedValue(new Error('network'))
    let cleared: boolean | undefined
    await act(async () => {
      cleared = await result.current.clearSL(stop.symbol, 'NFO', 'NRML')
    })
    expect(cleared).toBe(false)
    expect(result.current.slMap[key]).toBeDefined()
    expect(toast.error).toHaveBeenCalledWith(
      'Stop-loss not removed. It is still active. Try again.',
      undefined,
      { duration: Number.POSITIVE_INFINITY }
    )
  })

  it('an acknowledged removal reads the stops back, so one the server kept shows again', async () => {
    const { result } = await mounted([savedRow])
    await waitFor(() => expect(result.current.slMap[key]).toBeDefined())
    api.deleteSL.mockResolvedValue({ status: 'success', deleted: false })
    const reads = api.getSLStates.mock.calls.length
    await act(async () => {
      await result.current.clearSL(stop.symbol, 'NFO', 'NRML')
    })
    expect(api.getSLStates.mock.calls.length).toBe(reads + 1)
    await waitFor(() => expect(result.current.slMap[key]).toBeDefined())
  })
})
