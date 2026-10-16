import { Link } from 'react-router'

/**
 * Desktop departure from the web's Profile page (UI-05, UI-06). The web also
 * has an SMTP tab, for password-reset email, and a System tab, which checks
 * the file permissions of a server install and repairs them. OpenAlgo
 * Desktop sends no email (a forgotten password is replaced on the sign-in
 * page, KEY-01) and keeps its files in the operating system's app data
 * folder, so the server has neither feature and the tabs are left out. This
 * note says where a trader who knows the web finds those needs met.
 */
export function ProfileDesktopNote() {
  return (
    <p className="text-sm text-muted-foreground">
      OpenAlgo Desktop sends no email: a forgotten password is replaced from the sign-in page.
      Details about this computer and the app are on the{' '}
      <Link to="/admin/diagnostics" className="text-primary hover:underline">
        Diagnostics
      </Link>{' '}
      page.
    </p>
  )
}
