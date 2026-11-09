import { useQuery, useQueryClient } from '@tanstack/react-query'
import { BookOpen, Info, Loader2, Search, Settings } from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { Link, useNavigate, useSearchParams } from 'react-router'
import {
  BrokerConfigError,
  type BrokerEntry,
  brokerConfigApi,
  brokerStatus,
  sortBrokers,
} from '@/api/broker-config'
import { BrokerAuthSignOut } from '@/components/auth/BrokerAuthSignOut'
import { BrokerSwitchConfirm } from '@/components/auth/BrokerSwitchConfirm'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import {
  brokerConfigPath,
  brokerSignInTarget,
  DESKTOP_BROKER_SETUP_PATH,
  fetchLiveBroker,
  signInWithSavedKeys,
} from '@/lib/desktop'
import { useAuthStore } from '@/stores/authStore'
import { useBrokerStore } from '@/stores/brokerStore'

/**
 * Desktop: Select Broker, a card per broker (modelled on OpenBull's
 * BrokerSelect, an approved deviation from the web's single-broker page).
 *
 * Keys can be saved for many brokers; one broker is logged in at a time.
 * "Login with" starts each sign-in the way the server's catalogue says
 * (`sign_in`): the broker's own site, the in-app form, or one action with
 * the saved keys. Logging in to another broker while one is live asks first
 * and ends that session before the new sign-in starts.
 */

/**
 * Shown for a refused sign-in whose reason OpenAlgo no longer holds (read
 * already, expired, another session's) or never issued: the address is
 * built by whoever sent the link, so its text never reaches the page (as
 * on the web).
 */
const LOGIN_FAILED = 'The broker login did not complete. Try again.'

/** Brokers whose market data needs a paid data subscription (as on the web). */
const DATA_SUBSCRIPTION_NOTE: Record<string, string> = {
  zerodha: 'Zerodha requires an active Kite Connect data subscription for market data access.',
  dhan: 'Dhan requires an active Data API subscription for market data access.',
}

/**
 * Brokers whose live prices OpenAlgo makes up (maintainer, 2026-11-09): said
 * on the card itself, configured or not, so a trader never picks the feed
 * without seeing it (R107-18).
 */
const SIMULATED_PRICES_NOTE: Record<string, string> = {
  dhan_sandbox:
    'Simulated prices, not market prices: the Dhan sandbox streams none, so OpenAlgo makes them up as OpenAlgo web does.',
}

function StatusBadge({ broker }: { broker: BrokerEntry }) {
  const status = brokerStatus(broker)
  if (status === 'Active') return <Badge className="bg-green-600 text-white">Active</Badge>
  if (status === 'Configured') return <Badge variant="secondary">Configured</Badge>
  return <Badge variant="outline">Not Configured</Badge>
}

export default function BrokerSelect() {
  const { user } = useAuthStore()
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const [searchParams, setSearchParams] = useSearchParams()
  const [filter, setFilter] = useState('')
  const [starting, setStarting] = useState<string | null>(null)
  // The reason a sign-in failed, shown on that broker's card.
  const [cardError, setCardError] = useState<{ broker: string; message: string } | null>(null)
  const [pageError, setPageError] = useState<string | null>(null)
  // A login waiting for the trader to confirm ending the live session.
  const [pendingLogin, setPendingLogin] = useState<{ from: string; to: string } | null>(null)

  const list = useQuery({
    queryKey: ['broker-list'],
    queryFn: brokerConfigApi.list,
    staleTime: 0,
    retry: false,
  })

  // A broker sign-in that failed comes back here as
  // `/broker?error=<id>&broker=<id>`. Desktop: the id names the reason the
  // server kept for this browser session (the broker's own words), read
  // once; the web maps a fixed code to a fixed sentence instead.
  const [failure, setFailure] = useState<{ id: string; broker: string | null } | null>(null)
  useEffect(() => {
    const id = searchParams.get('error')
    if (!id) return
    setFailure({ id, broker: searchParams.get('broker') })
    // Shown once: a reload does not bring the old reason back.
    setSearchParams({}, { replace: true })
  }, [searchParams, setSearchParams])

  useEffect(() => {
    if (!failure) return
    let current = true
    void brokerConfigApi.signInFailure(failure.id).then((kept) => {
      if (!current) return
      const broker = kept?.broker ?? failure.broker
      const message = kept?.message ?? LOGIN_FAILED
      if (broker) setCardError({ broker, message })
      else setPageError(message)
    })
    return () => {
      current = false
    }
  }, [failure])

  useEffect(() => {
    if (list.error instanceof BrokerConfigError && list.error.status === 401) {
      navigate('/login', { replace: true })
    }
  }, [list.error, navigate])

  const brokers = useMemo(() => sortBrokers(list.data?.brokers ?? []), [list.data])
  const shown = useMemo(() => {
    const q = filter.trim().toLowerCase()
    if (!q) return brokers
    return brokers.filter((b) => b.name.toLowerCase().includes(q) || b.id.includes(q))
  }, [brokers, filter])
  const nameOf = (id: string) => brokers.find((b) => b.id === id)?.name ?? id

  // A card error for a broker that is not listed shows at the top instead.
  useEffect(() => {
    if (cardError && list.data && !list.data.brokers.some((b) => b.id === cardError.broker)) {
      setPageError(cardError.message)
      setCardError(null)
    }
  }, [cardError, list.data])

  // Start a sign-in the way the server says this broker signs in.
  const startSignIn = async (broker: BrokerEntry) => {
    setStarting(broker.id)
    setCardError(null)
    setPageError(null)
    const target = brokerSignInTarget(broker.id, broker.sign_in)
    if (target) {
      window.location.href = target
      return
    }
    if (broker.sign_in === 'saved_keys') {
      // This sign-in may replace the logged-in broker's session: an answer
      // about the old session's capabilities still in flight must never be
      // applied after it (STATE-01).
      const capabilities = useBrokerStore.getState()
      capabilities.clearCapabilities()
      const refusal = await signInWithSavedKeys(broker.id)
      if (refusal) {
        setCardError({ broker: broker.id, message: refusal })
        setStarting(null)
        // Refused, or the reply was lost: the server says which broker is
        // logged in now, not what this page assumed (FLOW-03), and the
        // capabilities are read again for it.
        await refresh()
        void capabilities.fetchCapabilities()
        return
      }
      window.location.href = '/dashboard'
      return
    }
    setCardError({ broker: broker.id, message: 'This broker cannot be logged in to from here.' })
    setStarting(null)
  }

  // One broker at a time. Logging in to another is confirmed first; the
  // server ends the live session only when the new sign-in completes, so
  // one that fails or is abandoned leaves the logged-in broker connected.
  const handleLogin = async (broker: BrokerEntry) => {
    const live = await fetchLiveBroker(list.data?.logged_in ?? null)
    if (live && live !== broker.id) {
      // Live automation or unsettled orders on the logged-in broker refuse
      // the switch outright (the server enforces it too): say why instead
      // of offering the confirm.
      const refusal = list.data?.switch_refusal
      if (refusal) {
        setCardError({ broker: broker.id, message: refusal })
        await refresh()
        return
      }
      setPendingLogin({ from: live, to: broker.id })
      return
    }
    await startSignIn(broker)
  }

  const confirmLogin = async () => {
    const pending = pendingLogin
    setPendingLogin(null)
    const broker = brokers.find((b) => b.id === pending?.to)
    if (!pending || !broker) return
    await startSignIn(broker)
  }

  // Read the broker list again (which broker is logged in, which are saved).
  const refresh = async () => {
    await queryClient.invalidateQueries({ queryKey: ['broker-list'] })
  }

  if (list.isLoading) {
    return (
      <div className="min-h-screen flex items-center justify-center">
        <Loader2 className="h-8 w-8 animate-spin" aria-label="Loading brokers" />
      </div>
    )
  }

  return (
    <div className="min-h-screen bg-background py-8 px-4">
      <div className="container mx-auto max-w-6xl space-y-6">
        <div className="flex flex-col gap-4 md:flex-row md:items-end md:justify-between">
          <div>
            <h1 className="text-2xl font-bold tracking-tight">Select Broker</h1>
            <p className="text-sm text-muted-foreground">
              {user?.username ? (
                <>
                  Welcome, <span className="font-medium">{user.username}</span>.{' '}
                </>
              ) : null}
              Save keys for as many brokers as you like; one broker is logged in at a time.
            </p>
          </div>
          <div className="flex items-center gap-3 text-sm">
            <Button asChild variant="outline" size="sm">
              <Link to={DESKTOP_BROKER_SETUP_PATH}>
                <Settings className="mr-2 h-4 w-4" />
                Broker Configuration
              </Link>
            </Button>
            <Button asChild variant="ghost" size="sm">
              <a href="https://docs.openalgo.in" target="_blank" rel="noopener noreferrer">
                <BookOpen className="mr-2 h-4 w-4" />
                Documentation
              </a>
            </Button>
            <BrokerAuthSignOut />
          </div>
        </div>

        {(pageError || list.error) && (
          <Alert variant="destructive">
            <AlertDescription>
              {pageError ??
                (list.error instanceof Error
                  ? list.error.message
                  : 'Could not load your brokers. Try again.')}
            </AlertDescription>
          </Alert>
        )}

        <div className="relative max-w-sm">
          <Search className="absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-muted-foreground" />
          <Input
            aria-label="Search brokers"
            placeholder="Search brokers"
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            className="pl-9"
          />
        </div>

        {shown.length === 0 && !list.error && (
          <p className="text-sm text-muted-foreground">No broker matches "{filter}".</p>
        )}

        <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
          {shown.map((broker) => {
            const error = cardError?.broker === broker.id ? cardError.message : null
            const note = DATA_SUBSCRIPTION_NOTE[broker.id]
            const simulated = SIMULATED_PRICES_NOTE[broker.id]
            return (
              <Card
                key={broker.id}
                data-testid={`broker-card-${broker.id}`}
                className="flex flex-col transition-shadow hover:shadow-md"
              >
                <CardHeader>
                  <div className="flex items-center justify-between gap-2">
                    <CardTitle className="text-lg">{broker.name}</CardTitle>
                    <StatusBadge broker={broker} />
                  </div>
                  <CardDescription>
                    {broker.exchanges.length > 0
                      ? `Exchanges: ${broker.exchanges.join(', ')}`
                      : 'Exchanges: not listed'}
                  </CardDescription>
                </CardHeader>
                <CardContent className="mt-auto space-y-3">
                  {simulated && (
                    <p className="flex gap-2 text-xs text-amber-700 dark:text-amber-400">
                      <Info className="h-4 w-4 shrink-0" />
                      {simulated}
                    </p>
                  )}
                  {note && (broker.configured || broker.active) && (
                    <p className="flex gap-2 text-xs text-amber-700 dark:text-amber-400">
                      <Info className="h-4 w-4 shrink-0" />
                      {note}
                    </p>
                  )}
                  {error && (
                    <Alert variant="destructive" role="alert">
                      <AlertDescription>{error}</AlertDescription>
                    </Alert>
                  )}
                  {broker.active ? (
                    <Button className="w-full" onClick={() => navigate('/dashboard')}>
                      Open Dashboard
                    </Button>
                  ) : broker.configured ? (
                    <Button
                      className="w-full"
                      onClick={() => handleLogin(broker)}
                      disabled={starting !== null}
                    >
                      {starting === broker.id ? (
                        <>
                          <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                          Connecting...
                        </>
                      ) : (
                        `Login with ${broker.name}`
                      )}
                    </Button>
                  ) : (
                    <Button asChild variant="outline" className="w-full">
                      <Link to={brokerConfigPath(broker.id)}>Configure</Link>
                    </Button>
                  )}
                  {broker.configured && (
                    <div className="text-center text-xs">
                      <Link
                        to={brokerConfigPath(broker.id)}
                        className="text-muted-foreground underline underline-offset-4 hover:text-foreground"
                        aria-label={`Edit ${broker.name} keys`}
                      >
                        Edit keys
                      </Link>
                    </div>
                  )}
                </CardContent>
              </Card>
            )
          })}
        </div>
      </div>

      <BrokerSwitchConfirm
        from={pendingLogin ? nameOf(pendingLogin.from) : null}
        to={pendingLogin ? nameOf(pendingLogin.to) : ''}
        onCancel={() => setPendingLogin(null)}
        onConfirm={confirmLogin}
      />
    </div>
  )
}
