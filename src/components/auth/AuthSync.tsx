import { useEffect, useState } from 'react'
import { useAutoLogout } from '@/hooks/useAutoLogout'
import { useAuthStore } from '@/stores/authStore'
import { useThemeStore } from '@/stores/themeStore'

interface AuthSyncProps {
  children: React.ReactNode
}

/**
 * AuthSync component that synchronizes Tauri backend session with Zustand store.
 * This ensures the React app knows about authentication state on startup.
 * Also syncs app mode (live/analyzer) from the settings.
 * Handles auto-logout at 3:00 AM IST for broker compliance.
 */
export function AuthSync({ children }: AuthSyncProps) {
  const [isChecking, setIsChecking] = useState(true)
  const { checkSession, isAuthenticated } = useAuthStore()
  const { syncAppMode } = useThemeStore()

  // Initialize auto-logout listener (3:00 AM IST compliance)
  useAutoLogout(isAuthenticated)

  useEffect(() => {
    const syncSession = async () => {
      try {
        // Check session with Tauri backend
        const isAuthenticated = await checkSession()

        if (isAuthenticated) {
          // Also sync app mode from settings
          await syncAppMode()
        }
      } catch (error) {
        console.error('Failed to sync session:', error)
        // On error, don't change auth state - let existing state persist
      } finally {
        setIsChecking(false)
      }
    }

    syncSession()
  }, [checkSession, syncAppMode])

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
