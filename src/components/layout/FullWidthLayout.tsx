import { Navigate, Outlet, useLocation } from 'react-router'
import { ModeSync } from '@/components/desktop/ModeSync'
import { UpdateNotice } from '@/components/desktop/UpdateNotice'
import { SocketProvider } from '@/components/socket/SocketProvider'
import { layoutRedirect } from '@/lib/desktop'
import { useAuthStore } from '@/stores/authStore'

/**
 * Full-width layout for apps like Playground that need maximum screen space.
 * No container constraints, minimal chrome.
 */
export function FullWidthLayout() {
  const { isAuthenticated, user } = useAuthStore()
  const { pathname } = useLocation()

  // Desktop: the same rule as Layout: /login when not signed in, /broker
  // when signed in with no broker logged in.
  const to = layoutRedirect(user, isAuthenticated, pathname)
  if (to) {
    return <Navigate to={to} replace />
  }

  return (
    <SocketProvider>
      <div className="h-screen bg-background flex flex-col overflow-hidden">
        <Outlet />
        {/* Desktop: the in-app update banner and dialog. */}
        <UpdateNotice />
        {/* Desktop: the Live/Analyze mode follows the server's (UI-01). */}
        <ModeSync />
      </div>
    </SocketProvider>
  )
}
