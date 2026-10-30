import { Home } from 'lucide-react'
import { Link } from 'react-router'
import { AccountControls } from '@/components/layout/AccountControls'
import { Button } from '@/components/ui/button'
import { useGuardedNavigate } from './FlowLeaveGuard'

/**
 * Desktop departure from the web's Flow editor header (lead follow-up
 * 2026-10-30). The web's editor draws its own copy of the account controls:
 * its mode switch shows no toast and no sandbox warning, and its profile menu
 * lacks the desktop's Check for Updates. Here the editor shows the same
 * AccountControls as the navbar and the /trading toolbar, after the web's
 * Dashboard link, with the profile menu going through the editor's guarded
 * navigate so unsaved changes are asked about first.
 */
export function FlowEditorAccountControls() {
  const navigate = useGuardedNavigate()
  return (
    <>
      <Button variant="ghost" size="sm" className="h-7 text-xs" asChild>
        <Link to="/dashboard">
          <Home className="h-3.5 w-3.5 mr-1.5" />
          Dashboard
        </Link>
      </Button>
      <AccountControls onNavigate={(href) => navigate(href)} />
    </>
  )
}
