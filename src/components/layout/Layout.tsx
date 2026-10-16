import { Navigate, Outlet, useLocation } from 'react-router'
import { ModeSync } from '@/components/desktop/ModeSync'
import { UpdateNotice } from '@/components/desktop/UpdateNotice'
import { SocketProvider } from '@/components/socket/SocketProvider'
import { layoutRedirect } from '@/lib/desktop'
import { useAuthStore } from '@/stores/authStore'
import { Footer } from './Footer'
import { MobileBottomNav } from './MobileBottomNav'
import { Navbar } from './Navbar'

export function Layout() {
  const { isAuthenticated, user } = useAuthStore()
  const { pathname } = useLocation()

  // AuthSync has already synced Flask session with Zustand store
  // So we just need to check the Zustand store state
  // Desktop: not signed in goes to /login; signed in with no broker logged
  // in goes straight to /broker (Profile and Server Settings stay open, to
  // free a port or reach the broker pages from Profile).
  const to = layoutRedirect(user, isAuthenticated, pathname)
  if (to) {
    return <Navigate to={to} replace />
  }

  return (
    <SocketProvider>
      <div className="min-h-screen bg-background flex flex-col">
        <Navbar />
        <main className="container mx-auto px-4 py-6 pb-24 md:pb-6 flex-1">
          <Outlet />
        </main>
        <Footer className="hidden md:block" />
        <MobileBottomNav />
        {/* Desktop: the in-app update banner and dialog. */}
        <UpdateNotice />
        {/* Desktop: the Live/Analyze mode follows the server's (UI-01). */}
        <ModeSync />
      </div>
    </SocketProvider>
  )
}

export function PublicLayout() {
  return (
    <div className="min-h-screen bg-background">
      <Outlet />
    </div>
  )
}
