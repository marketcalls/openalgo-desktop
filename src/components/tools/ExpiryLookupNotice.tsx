import { RefreshCw } from 'lucide-react'
import { Link } from 'react-router'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import type { ExpiryLookup } from '@/hooks/useExpiryLookup'
import { cn } from '@/lib/utils'

/**
 * What the option tools show while their expiry lookup is not ready: a
 * spinner only while it is actually running, otherwise a lasting sentence
 * with Retry and, where it helps, the page that fixes the cause.
 *
 * Desktop: the web pages have no such state (TOOLS-02).
 */
export function ExpiryLookupNotice({
  lookup,
  className,
}: {
  lookup: ExpiryLookup
  className?: string
}) {
  if (lookup.phase === 'idle' || lookup.phase === 'ready') return null
  if (lookup.phase === 'loading') {
    return (
      <div
        role="status"
        className={cn('flex items-center gap-2 text-sm text-muted-foreground', className)}
        data-testid="expiry-lookup-loading"
      >
        <div className="h-4 w-4 animate-spin rounded-full border-2 border-current border-t-transparent" />
        Loading expiry dates...
      </div>
    )
  }
  const title =
    lookup.phase === 'empty'
      ? 'No live expiry dates'
      : lookup.phase === 'master'
        ? 'Master contract not loaded'
        : lookup.phase === 'signin'
          ? 'Sign in again'
          : 'Expiry dates could not be loaded'
  return (
    <Alert variant={lookup.phase === 'empty' ? 'default' : 'warning'} className={className}>
      <AlertTitle>{title}</AlertTitle>
      <AlertDescription>
        <p>{lookup.message}</p>
        <div className="mt-3 flex flex-wrap items-center gap-3">
          <Button variant="outline" size="sm" onClick={lookup.retry}>
            <RefreshCw className="mr-2 h-4 w-4" />
            Retry
          </Button>
          {(lookup.phase === 'master' || lookup.phase === 'empty') && (
            <Link to="/master-contract" className="text-sm font-medium text-primary underline">
              Open Master Contract
            </Link>
          )}
          {lookup.phase === 'signin' && (
            <Link to="/login" className="text-sm font-medium text-primary underline">
              Sign in
            </Link>
          )}
        </div>
      </AlertDescription>
    </Alert>
  )
}

/** The Option Chain reads the chain with the API key; without one it cannot start. */
export function ApiKeyNotice() {
  return (
    <Alert variant="warning">
      <AlertTitle>API key needed</AlertTitle>
      <AlertDescription>
        <p>
          The option chain is loaded with your OpenAlgo API key, and none is set up. Generate one on
          the API Key page, then come back.
        </p>
        <div className="mt-3">
          <Link to="/apikey" className="text-sm font-medium text-primary underline">
            Open API Key
          </Link>
        </div>
      </AlertDescription>
    </Alert>
  )
}
