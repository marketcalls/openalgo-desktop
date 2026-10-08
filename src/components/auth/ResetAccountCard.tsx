import { useState } from 'react'
import { useNavigate } from 'react-router'
import { Button } from '@/components/ui/button'
import { isDesktopShell, resetAccountFromDesktop } from '@/lib/desktop'
import { showToast } from '@/utils/toast'

/**
 * Desktop: the last way back in when the password and the authenticator are
 * both lost. The reset runs only inside the OpenAlgo Desktop window, after
 * the trader confirms in a system dialog; a browser tab shows where to go.
 */
export function ResetAccountCard() {
  const navigate = useNavigate()
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const inShell = isDesktopShell()

  const onReset = async () => {
    setBusy(true)
    setError('')
    try {
      const outcome = await resetAccountFromDesktop()
      if (outcome.status === 'reset') {
        showToast.success(outcome.message, 'system')
        navigate('/setup', { replace: true })
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Your account could not be reset.')
    } finally {
      setBusy(false)
    }
  }

  return (
    <section
      aria-labelledby="reset-account-heading"
      className="rounded-lg border border-destructive/40 p-4 space-y-2 text-sm"
    >
      <h2 id="reset-account-heading" className="font-medium">
        Lost your password and your authenticator?
      </h2>
      <p className="text-muted-foreground">
        Reset account removes your OpenAlgo login, API key and saved broker keys, ends your broker
        session, stops AI client tokens, unlinks Telegram and WhatsApp, and gives your strategy and
        Chartink webhooks new addresses. Your trade and strategy history stays. You then create a
        new account and connect your broker again.
      </p>
      {inShell ? (
        <Button
          type="button"
          variant="destructive"
          className="w-full"
          onClick={onReset}
          disabled={busy}
        >
          {busy ? 'Waiting for your confirmation...' : 'Reset account'}
        </Button>
      ) : (
        <p>
          For your safety this works only in the OpenAlgo Desktop window on the computer where it is
          installed. Open the app there and choose Reset account on this page.
        </p>
      )}
      {error && (
        <p role="alert" className="text-destructive">
          {error}
        </p>
      )}
    </section>
  )
}
