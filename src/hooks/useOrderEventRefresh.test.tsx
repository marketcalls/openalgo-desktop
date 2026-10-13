import { renderHook } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

type Handler = (...args: unknown[]) => void

/** A stand-in for the shared Socket.IO connection: on/off/emit only. */
function fakeSocket() {
  const handlers = new Map<string, Set<Handler>>()
  return {
    on(event: string, fn: Handler) {
      if (!handlers.has(event)) handlers.set(event, new Set())
      handlers.get(event)?.add(fn)
    },
    off(event: string, fn: Handler) {
      handlers.get(event)?.delete(fn)
    },
    emit(event: string) {
      for (const fn of handlers.get(event) ?? []) fn()
    },
    listeners(event: string) {
      return handlers.get(event)?.size ?? 0
    },
  }
}

const socketRef: { current: ReturnType<typeof fakeSocket> | null } = { current: null }
vi.mock('@/components/socket/SocketProvider', () => ({
  useSocketContext: () => ({ socket: socketRef.current }),
}))

import { useOrderEventRefresh } from './useOrderEventRefresh'

describe('useOrderEventRefresh', () => {
  beforeEach(() => {
    vi.useFakeTimers()
    socketRef.current = fakeSocket()
  })
  afterEach(() => {
    vi.useRealTimers()
  })

  it('refreshes when the broker reports a resting order filled (order_update)', () => {
    const refresh = vi.fn()
    renderHook(() => useOrderEventRefresh(refresh, { events: ['order_update'], delay: 150 }))
    socketRef.current?.emit('order_update')
    vi.advanceTimersByTime(150)
    expect(refresh).toHaveBeenCalledTimes(1)
  })

  it('collapses a burst of events into one refresh', () => {
    const refresh = vi.fn()
    renderHook(() =>
      useOrderEventRefresh(refresh, { events: ['order_event', 'order_update'], delay: 150 })
    )
    for (let i = 0; i < 5; i++) socketRef.current?.emit('order_update')
    socketRef.current?.emit('order_event')
    vi.advanceTimersByTime(150)
    expect(refresh).toHaveBeenCalledTimes(1)
    // An event after that refresh fired schedules the next one.
    socketRef.current?.emit('order_update')
    vi.advanceTimersByTime(150)
    expect(refresh).toHaveBeenCalledTimes(2)
  })

  it('refreshes once the connection comes back, and not when told not to', () => {
    const refresh = vi.fn()
    renderHook(() => useOrderEventRefresh(refresh, { delay: 100 }))
    socketRef.current?.emit('connect')
    vi.advanceTimersByTime(100)
    expect(refresh).toHaveBeenCalledTimes(1)

    const quiet = vi.fn()
    renderHook(() => useOrderEventRefresh(quiet, { delay: 100, refreshOnReconnect: false }))
    socketRef.current?.emit('connect')
    vi.advanceTimersByTime(100)
    expect(quiet).not.toHaveBeenCalled()
  })

  it('does not refresh after the page has closed, and removes its listeners', () => {
    const refresh = vi.fn()
    const { unmount } = renderHook(() =>
      useOrderEventRefresh(refresh, { events: ['order_update'], delay: 150 })
    )
    socketRef.current?.emit('order_update')
    unmount()
    vi.advanceTimersByTime(500)
    expect(refresh).not.toHaveBeenCalled()
    expect(socketRef.current?.listeners('order_update')).toBe(0)
    expect(socketRef.current?.listeners('connect')).toBe(0)
  })
})
