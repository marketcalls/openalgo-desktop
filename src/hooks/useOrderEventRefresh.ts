/**
 * Order Event Refresh hook for OpenAlgo Desktop
 *
 * In the desktop version, real-time updates come from Tauri events
 * instead of Socket.IO. This hook provides a compatible interface.
 */

import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { useEffect, useRef } from 'react'

/**
 * Supported event types for order-related updates
 */
export type OrderEventType =
  | 'order_event'
  | 'analyzer_update'
  | 'close_position_event'
  | 'cancel_order_event'
  | 'modify_order_event'

/**
 * Configuration options for useOrderEventRefresh hook
 */
export interface UseOrderEventRefreshOptions {
  /** Events to listen for (default: ['order_event', 'analyzer_update']) */
  events?: OrderEventType[]
  /** Delay in ms before calling refresh function (default: 500) */
  delay?: number
  /** Whether the hook is enabled (default: true) */
  enabled?: boolean
}

/**
 * Centralized hook for order event listeners using Tauri events.
 *
 * Automatically sets up Tauri event listeners and calls the refresh
 * function with an optional delay when events occur.
 */
export function useOrderEventRefresh(
  refreshFn: () => void,
  options: UseOrderEventRefreshOptions = {}
): void {
  const { events = ['order_event', 'analyzer_update'], delay = 500, enabled = true } = options

  const refreshFnRef = useRef(refreshFn)

  // Keep refresh function reference up to date
  useEffect(() => {
    refreshFnRef.current = refreshFn
  }, [refreshFn])

  useEffect(() => {
    if (!enabled) return

    const unlisteners: UnlistenFn[] = []

    // Create handler for each event type
    const handleEvent = () => {
      // Delay slightly to allow backend to process the event
      setTimeout(() => refreshFnRef.current(), delay)
    }

    // Register listeners for all specified events
    events.forEach((event) => {
      listen(event, handleEvent).then((unlisten) => unlisteners.push(unlisten))
    })

    // Cleanup on unmount
    return () => {
      unlisteners.forEach((unlisten) => unlisten())
    }
  }, [events, delay, enabled])
}

/**
 * Hook to get connection status for Tauri events.
 * Always returns connected since Tauri events are always available.
 */
export function useSocketConnection(_enabled = true): {
  socket: null
  isConnected: boolean
} {
  return {
    socket: null,
    isConnected: true, // Tauri events are always available
  }
}
