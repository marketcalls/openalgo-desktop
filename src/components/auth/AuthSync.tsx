import { useCallback, useEffect, useRef, useState } from 'react'
import { useLocation } from 'react-router'
import { useAuthStore } from '@/stores/authStore'
import { useBrokerStore } from '@/stores/brokerStore'
import { useSessionStore } from '@/stores/sessionStore'
import { useThemeStore } from '@/stores/themeStore'

interface AuthSyncProps {
  children: React.ReactNode
}

/** The public pages and the broker sign-in pages, which need no broker. */
export function beforeBroker(pathname: string): boolean {
  return (
    /^\/(faq|setup|login|reset-password|download|error|rate-limited)?\/?$/.test(pathname) ||
    pathname === '/broker' ||
    pathname.startsWith('/broker/') ||
    /^\/[^/]+\/auth\/?$/.test(pathname)
  )
}

/**
 * AuthSync component that synchronizes Flask session with Zustand store.
 * This ensures the React app knows about authentication state from OAuth callbacks.
 * Also syncs app mode (live/analyzer) from the backend.
 */
export function AuthSync({ children }: AuthSyncProps) {
  const [isChecking, setIsChecking] = useState(true)
  const { setUser, setApiKey, logout } = useAuthStore()
  const isAuthenticated = useAuthStore((s) => s.isAuthenticated)
  const apiKey = useAuthStore((s) => s.apiKey)
  const { fetchCapabilities, clearCapabilities } = useBrokerStore()
  const { setActiveSessionCount } = useSessionStore()
  const { syncAppMode } = useThemeStore()
  const { pathname } = useLocation()
  const syncing = useRef(false)

  const syncSession = useCallback(async () => {
    if (syncing.current) return
    syncing.current = true
    try {
      const response = await fetch('/auth/session-status', {
        credentials: 'include',
      })

      if (response.ok) {
        const data = await response.json()

        if (data.status === 'success' && data.logged_in && data.broker) {
          // Flask session is authenticated with broker - sync to Zustand
          setUser({
            username: data.user,
            broker: data.broker,
            isLoggedIn: true,
            loginTime: new Date().toISOString(),
          })
          // Store the API key for trading API calls
          if (data.api_key) {
            setApiKey(data.api_key)
          }
          // Fetch broker capabilities (exchanges, type, features)
          await fetchCapabilities()
          // Also sync app mode from backend
          await syncAppMode()
          // Sync active session count
          if (data.active_sessions !== undefined) {
            setActiveSessionCount(data.active_sessions)
          }
        } else if (data.status === 'success' && data.authenticated && !data.logged_in) {
          // User is logged in but hasn't connected broker yet
          setUser({
            username: data.user,
            broker: null,
            isLoggedIn: false,
            loginTime: null,
          })
          clearCapabilities()
        } else {
          // Not authenticated or status is not success - clear Zustand store
          logout()
          clearCapabilities()
        }
      } else {
        // Any non-OK response (401, 500, etc.) - clear Zustand store
        logout()
        clearCapabilities()
      }
    } catch (_error) {
      // On error, don't change auth state - let existing state persist
    } finally {
      syncing.current = false
      setIsChecking(false)
    }
  }, [
    setUser,
    setApiKey,
    logout,
    fetchCapabilities,
    clearCapabilities,
    syncAppMode,
    setActiveSessionCount,
  ])

  useEffect(() => {
    syncSession()
  }, [syncSession])

  // A sign-in, or a broker login on its card, changes the session without
  // reloading the page, so the check above (made when the window opened)
  // never saw it and the API key that the books, quotes and order pages read
  // stays empty. While signed in without a key, check again on each page
  // change; it stops once the session carries the key. The web reloads after
  // a broker login, which hides this there.
  // Only on pages that need a broker: on the sign-in and broker pages a
  // session without a broker is expected, and syncing there would mark the
  // trader signed out of the app mid-flow.
  useEffect(() => {
    if (isChecking || !isAuthenticated || apiKey || beforeBroker(pathname)) return
    syncSession()
  }, [isChecking, isAuthenticated, apiKey, pathname, syncSession])

  // Show nothing while checking - prevents flash of wrong content
  if (isChecking) {
    return (
      <div className="min-h-screen flex items-center justify-center">
        <div className="animate-spin rounded-full h-8 w-8 border-b-2 border-primary"></div>
      </div>
    )
  }

  return <>{children}</>
}
