/**
 * Auto-logout hook for OpenAlgo Desktop
 *
 * Listens to Tauri events for scheduled auto-logout at 3:00 AM IST.
 * This is a compliance requirement for Indian brokers to ensure fresh
 * authentication each trading day.
 *
 * Warning schedule:
 * - 30 minutes before: Info toast
 * - 15 minutes before: Info toast
 * - 5 minutes before: Warning toast
 * - 1 minute before: Critical toast
 * - 0: Execute logout and redirect to login
 */

import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { useEffect, useRef } from 'react'
import { useNavigate } from 'react-router-dom'
import { toast } from 'sonner'
import { useAuthStore } from '@/stores/authStore'

/**
 * Auto-logout warning event payload from Rust backend
 */
interface AutoLogoutWarningPayload {
  minutes_remaining: number
  message: string
}

/**
 * Auto-logout event payload from Rust backend
 */
interface AutoLogoutPayload {
  reason: string
  timestamp: string
}

/**
 * Get toast type based on minutes remaining
 */
function getToastType(minutes: number): 'info' | 'warning' | 'error' {
  if (minutes <= 1) return 'error'
  if (minutes <= 5) return 'warning'
  return 'info'
}

/**
 * Hook to handle scheduled auto-logout at 3:00 AM IST.
 *
 * This hook should be used at the root of the application (e.g., AuthSync)
 * to ensure auto-logout is handled globally.
 *
 * @param enabled - Whether the hook is active (default: true)
 */
export function useAutoLogout(enabled = true): void {
  const navigate = useNavigate()
  const { logout, isAuthenticated } = useAuthStore()
  const logoutRef = useRef(logout)
  const navigateRef = useRef(navigate)

  // Keep refs up to date
  useEffect(() => {
    logoutRef.current = logout
    navigateRef.current = navigate
  }, [logout, navigate])

  useEffect(() => {
    // Only listen when enabled and authenticated
    if (!enabled || !isAuthenticated) {
      return
    }

    const unlisteners: UnlistenFn[] = []

    // Listen for warning events (30, 15, 5, 1 minutes before)
    listen<AutoLogoutWarningPayload>('auto_logout_warning', (event) => {
      const { minutes_remaining, message } = event.payload
      const toastType = getToastType(minutes_remaining)

      const toastOptions = {
        duration: 10000, // Show for 10 seconds
        id: 'auto-logout-warning', // Prevent duplicate toasts
      }

      if (toastType === 'error') {
        toast.error(message, toastOptions)
      } else if (toastType === 'warning') {
        toast.warning(message, toastOptions)
      } else {
        toast.info(message, toastOptions)
      }
    }).then((unlisten) => unlisteners.push(unlisten))

    // Listen for actual logout event
    listen<AutoLogoutPayload>('auto_logout', async (event) => {
      const { reason } = event.payload

      // Show final notification
      toast.info(reason, {
        duration: 5000,
        id: 'auto-logout-executed',
      })

      // Perform logout
      try {
        await logoutRef.current()
      } catch (error) {
        console.error('Auto-logout error:', error)
      }

      // Navigate to login with reason
      navigateRef.current('/login', {
        replace: true,
        state: { reason: 'auto_logout' },
      })
    }).then((unlisten) => unlisteners.push(unlisten))

    // Cleanup on unmount
    return () => {
      unlisteners.forEach((unlisten) => unlisten())
    }
  }, [enabled, isAuthenticated])
}

/**
 * Hook to display auto-logout reason on the login page.
 * Call this in the Login component to show a message when the user
 * was logged out due to the scheduled auto-logout.
 */
export function useAutoLogoutReason(): string | null {
  // This hook could be enhanced to read from router state
  // For now, the Login component can check location.state directly
  return null
}

export default useAutoLogout
