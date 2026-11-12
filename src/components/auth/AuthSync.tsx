import { useCallback, useEffect, useRef, useState } from 'react'
import { useLocation } from 'react-router'
import { useAuthStore } from '@/stores/authStore'
import { useBrokerStore } from '@/stores/brokerStore'
import { useSessionStore } from '@/stores/sessionStore'
import { useThemeStore } from '@/stores/themeStore'

interface SessionStatus {
  status?: string
  authenticated?: boolean
  logged_in?: boolean
  user?: string
  broker?: string
  api_key?: string
  active_sessions?: number
}

async function fetchSessionStatus(): Promise<SessionStatus | null> {
  const response = await fetch('/auth/session-status', { credentials: 'include' })
  if (!response.ok) return null
  return (await response.json()) as SessionStatus
}

function isBrokerSession(data: SessionStatus): boolean {
  return data.status === 'success' && !!data.logged_in && !!data.broker
}

/**
 * Copies a broker-logged-in session into the stores: user, API key, broker
 * capabilities, app mode and session count. Everything that reads the API key
 * from the auth store (the order tickets, live quotes) depends on this.
 */
async function applyBrokerSession(data: SessionStatus): Promise<void> {
  useAuthStore.getState().setUser({
    username: data.user ?? '',
    broker: data.broker ?? null,
    isLoggedIn: true,
    loginTime: new Date().toISOString(),
  })
  if (data.api_key) {
    useAuthStore.getState().setApiKey(data.api_key)
  }
  await useBrokerStore.getState().fetchCapabilities()
  await useThemeStore.getState().syncAppMode()
  if (data.active_sessions !== undefined) {
    useSessionStore.getState().setActiveSessionCount(data.active_sessions)
  }
}

/**
 * Desktop: the server has been asked for the API key since the last logout,
 * so a page held back for it (AuthSync below) goes ahead, with the key or
 * without one: the trader may not have generated one yet, or the server
 * could not be asked. A logout starts the wait again.
 */
function markApiKeyChecked(): void {
  const auth = useAuthStore.getState()
  if (auth.isAuthenticated && !auth.apiKeyChecked) {
    useAuthStore.setState({ apiKeyChecked: true })
  }
}

/**
 * Re-reads the session after a broker login that completes inside the app
 * (the TOTP and Samco pages). AuthSync only runs on a full page load, and
 * those logins move on without one, so without this the API key, capabilities
 * and app mode stay unset until the trader reloads the page. Returns whether
 * the server reported a broker session.
 */
export async function syncBrokerSession(): Promise<boolean> {
  try {
    const data = await fetchSessionStatus()
    if (!data || !isBrokerSession(data)) return false
    await applyBrokerSession(data)
    markApiKeyChecked()
    return true
  } catch {
    // The next page load syncs it.
    return false
  }
}

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
  const isAuthenticated = useAuthStore((s) => s.isAuthenticated)
  const apiKey = useAuthStore((s) => s.apiKey)
  const apiKeyChecked = useAuthStore((s) => s.apiKeyChecked)
  const { pathname } = useLocation()
  const syncing = useRef(false)

  const syncSession = useCallback(async () => {
    if (syncing.current) return
    syncing.current = true
    const { setUser, logout } = useAuthStore.getState()
    const { clearCapabilities } = useBrokerStore.getState()
    try {
      const data = await fetchSessionStatus()

      if (data && isBrokerSession(data)) {
        // Flask session is authenticated with broker - sync to Zustand
        await applyBrokerSession(data)
      } else if (data && data.status === 'success' && data.authenticated && !data.logged_in) {
        // User is logged in but hasn't connected broker yet
        setUser({
          username: data.user ?? '',
          broker: null,
          isLoggedIn: false,
          loginTime: null,
        })
        clearCapabilities()
      } else {
        // Not authenticated, status is not success, or a non-OK response
        // (401, 500, etc.) - clear Zustand store
        logout()
        clearCapabilities()
      }
    } catch (_error) {
      // On error, don't change auth state - let existing state persist
    } finally {
      syncing.current = false
      // Desktop: a page held back for the key (below) goes ahead now.
      markApiKeyChecked()
      setIsChecking(false)
    }
  }, [])

  useEffect(() => {
    syncSession()
  }, [syncSession])

  // Desktop: a sign-in that resumes the broker session changes it without
  // reloading the page, so the check above (made when the window opened)
  // never saw it and the API key that the books, quotes and order pages read
  // stays empty. While signed in without a key, check again on each page
  // change; it stops once the session carries the key. A broker login on its
  // card reads the session itself (syncBrokerSession), as on the web.
  // Only on pages that need a broker: on the sign-in and broker pages a
  // session without a broker is expected, and syncing there would mark the
  // trader signed out of the app mid-flow.
  useEffect(() => {
    if (isChecking || !isAuthenticated || apiKey || beforeBroker(pathname)) return
    syncSession()
  }, [isChecking, isAuthenticated, apiKey, pathname, syncSession])

  // Desktop: the API key is kept in memory only (authStore), so after a
  // sign-in that did not reload the page it is not there yet. A page that
  // needs a broker waits for the check above, so the books, quotes and order
  // forms never start without a key the trader has (an empty book, a
  // "generate an API key" notice).
  const awaitingKey = isAuthenticated && !apiKey && !apiKeyChecked && !beforeBroker(pathname)

  // Show nothing while checking - prevents flash of wrong content
  if (isChecking || awaitingKey) {
    return (
      <div className="min-h-screen flex items-center justify-center">
        <div className="animate-spin rounded-full h-8 w-8 border-b-2 border-primary"></div>
      </div>
    )
  }

  return <>{children}</>
}
