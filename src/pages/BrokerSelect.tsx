import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { open } from '@tauri-apps/plugin-shell'
import { BookOpen, Edit2, ExternalLink, Key, Loader2, Settings, Trash2 } from 'lucide-react'
import { useEffect, useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { toast } from 'sonner'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@/components/ui/alert-dialog'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { useAuthStore } from '@/stores/authStore'

interface BrokerInfo {
  id: string
  name: string
  auth_type: string
  has_credentials: boolean
}

interface BrokerConfigResponse {
  status: string
  broker_name: string | null
  broker_api_key: string | null
  redirect_url: string
  available_brokers: BrokerInfo[]
}

interface WebhookConfig {
  enabled: boolean
  host: string
  port: number
  ngrok_url: string | null
  webhook_secret: string | null
}

interface OAuthCallbackPayload {
  broker_id: string
  success: boolean
  auth_code: string | null
  message: string
}

interface BrokerLoginResponse {
  success: boolean
  broker_id: string
  user_id: string
  user_name?: string
}

// Helper function to get Flattrade API key
function getFlattradeApiKey(fullKey: string): string {
  if (!fullKey) return ''
  const parts = fullKey.split(':::')
  return parts.length > 1 ? parts[1] : fullKey
}

// Generate random state for OAuth
function generateRandomState(): string {
  const length = 16
  const chars = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789'
  let result = ''
  for (let i = 0; i < length; i++) {
    result += chars.charAt(Math.floor(Math.random() * chars.length))
  }
  return result
}

export default function BrokerSelect() {
  const navigate = useNavigate()
  const { user, setUser } = useAuthStore()
  const [selectedBroker, setSelectedBroker] = useState<string>('')
  const [isLoading, setIsLoading] = useState(true)
  const [isSubmitting, setIsSubmitting] = useState(false)
  const [isSavingCredentials, setIsSavingCredentials] = useState(false)
  const [isDeletingCredentials, setIsDeletingCredentials] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [brokerConfig, setBrokerConfig] = useState<BrokerConfigResponse | null>(null)
  const [showCredentialsDialog, setShowCredentialsDialog] = useState(false)
  const [showDeleteDialog, setShowDeleteDialog] = useState(false)
  const [isEditMode, setIsEditMode] = useState(false)
  const [credentialsForm, setCredentialsForm] = useState({
    apiKey: '',
    apiSecret: '',
    clientId: '',
  })
  const [webhookConfig, setWebhookConfig] = useState<WebhookConfig | null>(null)
  const [oauthState, setOauthState] = useState<string | null>(null)

  const fetchBrokerConfig = async () => {
    try {
      setIsLoading(true)
      const data = await invoke<BrokerConfigResponse>('get_broker_config')

      if (data.status === 'success') {
        setBrokerConfig(data)
        // Auto-select the configured broker if any
        if (data.broker_name) {
          setSelectedBroker(data.broker_name)
        }
      } else {
        setError('Failed to load broker configuration')
      }
    } catch (err) {
      console.error('Failed to load broker config:', err)
      setError('Failed to load broker configuration')
    } finally {
      setIsLoading(false)
    }
  }

  useEffect(() => {
    fetchBrokerConfig()

    // Fetch webhook config for redirect URL
    invoke<WebhookConfig>('get_webhook_config')
      .then(setWebhookConfig)
      .catch((err) => console.error('Failed to load webhook config:', err))

    // Listen for OAuth callback events
    const unlistenPromise = listen<OAuthCallbackPayload>('oauth_callback', async (event) => {
      const { broker_id, success, auth_code, message } = event.payload
      console.log('Received OAuth callback:', { broker_id, success, auth_code, message })

      if (!success || !auth_code) {
        toast.error(message || 'OAuth callback failed')
        setIsSubmitting(false)
        return
      }

      try {
        toast.info('Authenticating with broker...')

        // Get broker credentials
        const creds = await invoke<{
          broker_id: string
          api_key: string
          api_secret: string | null
          client_id: string | null
        } | null>('get_broker_credentials_for_edit', { brokerId: broker_id })

        if (!creds) {
          toast.error('Broker credentials not found')
          setIsSubmitting(false)
          return
        }

        // Call broker login with auth code
        const response = await invoke<BrokerLoginResponse>('broker_login', {
          request: {
            broker_id,
            credentials: {
              api_key: creds.api_key,
              api_secret: creds.api_secret,
              client_id: creds.client_id,
              auth_code: auth_code,
            },
          },
        })

        console.log('Broker login response:', response)

        // Backend returns { success: boolean, broker_id, user_id, user_name }
        if (response.success) {
          // Update auth store with broker info so Layout doesn't redirect back
          if (user) {
            setUser({
              ...user,
              broker: response.user_name || response.user_id,
              brokerId: response.broker_id,
            })
          }
          toast.success(`Connected to ${broker_id} as ${response.user_name || response.user_id}`)

          // Trigger master contract download in background
          toast.info('Downloading master contracts...')
          invoke<number>('refresh_symbol_master')
            .then((count) => {
              toast.success(`Master contracts loaded: ${count} symbols`)
            })
            .catch((err) => {
              console.error('Failed to download master contracts:', err)
              toast.error('Failed to download master contracts. Try refreshing from Dashboard.')
            })

          navigate('/dashboard')
        } else {
          toast.error('Failed to connect to broker')
        }
      } catch (err) {
        console.error('OAuth login error:', err)
        const errorMessage =
          err && typeof err === 'object' && 'message' in err
            ? (err as { message: string }).message
            : 'Failed to authenticate with broker'
        toast.error(errorMessage)
      } finally {
        setIsSubmitting(false)
        setOauthState(null)
      }
    })

    return () => {
      unlistenPromise.then((unlisten) => unlisten())
    }
  }, [oauthState, navigate])

  const selectedBrokerInfo = brokerConfig?.available_brokers.find((b) => b.id === selectedBroker)

  const handleSaveCredentials = async () => {
    if (!selectedBroker || !credentialsForm.apiKey) {
      setError('API Key is required')
      return
    }

    setIsSavingCredentials(true)
    try {
      await invoke('save_broker_credentials', {
        request: {
          broker_id: selectedBroker,
          api_key: credentialsForm.apiKey,
          api_secret: credentialsForm.apiSecret || null,
          client_id: credentialsForm.clientId || null,
        },
      })

      toast.success(isEditMode ? 'Broker credentials updated successfully' : 'Broker credentials saved successfully')
      setShowCredentialsDialog(false)
      setCredentialsForm({ apiKey: '', apiSecret: '', clientId: '' })
      setIsEditMode(false)

      // Refresh broker config to update has_credentials status
      await fetchBrokerConfig()
    } catch (err) {
      console.error('Failed to save credentials:', err)
      const errorMessage =
        err && typeof err === 'object' && 'message' in err
          ? (err as { message: string }).message
          : 'Failed to save credentials'
      setError(errorMessage)
    } finally {
      setIsSavingCredentials(false)
    }
  }

  const handleEditCredentials = async () => {
    if (!selectedBroker) return

    setIsEditMode(true)
    setError(null)

    try {
      // Fetch existing credentials to pre-fill the form
      const existingCreds = await invoke<{
        broker_id: string
        api_key: string
        api_secret: string | null
        client_id: string | null
      } | null>('get_broker_credentials_for_edit', { brokerId: selectedBroker })

      if (existingCreds) {
        setCredentialsForm({
          apiKey: existingCreds.api_key,
          apiSecret: existingCreds.api_secret || '',
          clientId: existingCreds.client_id || '',
        })
      } else {
        setCredentialsForm({ apiKey: '', apiSecret: '', clientId: '' })
      }
    } catch (err) {
      console.error('Failed to fetch credentials:', err)
      setCredentialsForm({ apiKey: '', apiSecret: '', clientId: '' })
    }

    setShowCredentialsDialog(true)
  }

  const handleDeleteCredentials = async () => {
    if (!selectedBroker) return

    setIsDeletingCredentials(true)
    try {
      await invoke('delete_broker_credentials', { brokerId: selectedBroker })
      toast.success('Broker credentials deleted successfully')
      setShowDeleteDialog(false)

      // Refresh broker config to update has_credentials status
      await fetchBrokerConfig()
    } catch (err) {
      console.error('Failed to delete credentials:', err)
      const errorMessage =
        err && typeof err === 'object' && 'message' in err
          ? (err as { message: string }).message
          : 'Failed to delete credentials'
      toast.error(errorMessage)
    } finally {
      setIsDeletingCredentials(false)
    }
  }

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault()

    if (!selectedBroker) {
      setError('Please select a broker')
      return
    }

    // Check if broker has credentials
    const broker = brokerConfig?.available_brokers.find((b) => b.id === selectedBroker)
    if (!broker?.has_credentials) {
      setIsEditMode(false)
      setShowCredentialsDialog(true)
      return
    }

    setIsSubmitting(true)
    setError(null)

    try {
      // For TOTP brokers, navigate to TOTP form
      if (broker.auth_type === 'totp') {
        navigate(`/broker/${selectedBroker}/totp`)
        return
      }

      // For OAuth brokers, initiate OAuth flow
      if (broker.auth_type === 'oauth') {
        console.log('Starting OAuth flow for broker:', selectedBroker)

        // Check if webhook server is enabled (required for OAuth callback)
        if (!webhookConfig?.enabled) {
          setError('The webhook server must be enabled to use OAuth login. Please go to Settings > Server Settings and enable the webhook server.')
          toast.error('Webhook server is not enabled', {
            description: 'Enable it in Settings > Server Settings to use OAuth login.',
            duration: 5000,
          })
          setIsSubmitting(false)
          return
        }

        // Get broker credentials for API key
        let creds: {
          broker_id: string
          api_key: string
          api_secret: string | null
          client_id: string | null
        } | null = null

        try {
          creds = await invoke<typeof creds>('get_broker_credentials_for_edit', { brokerId: selectedBroker })
          console.log('Got credentials:', creds ? 'yes' : 'no')
        } catch (credErr) {
          console.error('Failed to get credentials:', credErr)
          setError(`Failed to get credentials: ${credErr}`)
          setIsSubmitting(false)
          return
        }

        if (!creds) {
          setError('Broker credentials not found. Please configure credentials first.')
          setIsSubmitting(false)
          return
        }

        // Generate redirect URL from webhook config
        let redirectUrl: string
        if (webhookConfig?.ngrok_url) {
          redirectUrl = `${webhookConfig.ngrok_url}/${selectedBroker}/callback`
        } else if (webhookConfig) {
          redirectUrl = `http://${webhookConfig.host}:${webhookConfig.port}/${selectedBroker}/callback`
        } else {
          redirectUrl = `http://127.0.0.1:5000/${selectedBroker}/callback`
        }
        console.log('Redirect URL:', redirectUrl)

        // Generate state for security
        const state = generateRandomState()
        setOauthState(state)

        // Build OAuth URL based on broker
        let authUrl: string

        if (selectedBroker === 'fyers') {
          // Fyers OAuth URL format
          // API key format: APP_ID-100 (e.g., XYZ123-100)
          const appId = creds.api_key
          authUrl = `https://api-t1.fyers.in/api/v3/generate-authcode?client_id=${encodeURIComponent(appId)}&redirect_uri=${encodeURIComponent(redirectUrl)}&response_type=code&state=${state}`
        } else if (selectedBroker === 'zerodha') {
          // Zerodha OAuth URL format
          authUrl = `https://kite.zerodha.com/connect/login?v=3&api_key=${encodeURIComponent(creds.api_key)}&redirect_uri=${encodeURIComponent(redirectUrl)}&state=${state}`
        } else {
          toast.error(`OAuth not supported for ${broker.name}`)
          setIsSubmitting(false)
          return
        }

        // Open browser with OAuth URL
        toast.info('Opening browser for authentication...')
        console.log('Opening URL:', authUrl)

        try {
          await open(authUrl)
          // Keep submitting state true - will be reset when we receive callback
          toast.info('Please complete authentication in your browser')
        } catch (openErr) {
          console.error('Failed to open browser:', openErr)
          toast.error(`Failed to open browser: ${openErr}`)
          // Fallback: show the URL for manual copy
          toast.info('Copy this URL and open it manually in your browser')
          navigator.clipboard.writeText(authUrl)
          setError(`Could not open browser automatically. URL copied to clipboard.`)
        }
        return
      }

      // Unknown auth type
      toast.error(`Unknown authentication type: ${broker.auth_type}`)
    } catch (err) {
      console.error('Broker login error:', err)
      const errorMessage = err instanceof Error ? err.message : String(err)
      setError(`Failed to initiate broker login: ${errorMessage}`)
      setIsSubmitting(false)
    }
  }

  if (isLoading) {
    return (
      <div className="min-h-screen flex items-center justify-center">
        <Loader2 className="h-8 w-8 animate-spin" />
      </div>
    )
  }

  return (
    <div className="min-h-screen flex items-center justify-center py-8 px-4">
      <div className="container max-w-6xl">
        <div className="flex flex-col lg:flex-row items-center justify-between gap-8 lg:gap-16">
          {/* Right side broker form - Shown first on mobile */}
          <Card className="w-full max-w-md shadow-xl order-1 lg:order-2">
            <CardHeader className="text-center">
              <div className="flex justify-center mb-4">
                <img src="/logo.png" alt="OpenAlgo" className="h-20 w-20" />
              </div>
              <CardTitle className="text-2xl">Connect Your Trading Account</CardTitle>
              <CardDescription>
                Welcome, <span className="font-medium">{user?.username}</span>!
              </CardDescription>
            </CardHeader>
            <CardContent>
              {error && (
                <Alert variant="destructive" className="mb-4">
                  <AlertDescription>{error}</AlertDescription>
                </Alert>
              )}

              <form onSubmit={handleSubmit} className="space-y-6">
                <div className="space-y-2">
                  <Label htmlFor="broker-select" className="block text-center">
                    Select Your Broker
                  </Label>
                  <Select
                    value={selectedBroker}
                    onValueChange={setSelectedBroker}
                    disabled={isSubmitting}
                  >
                    <SelectTrigger id="broker-select" className="w-full">
                      <SelectValue placeholder="Select a Broker" />
                    </SelectTrigger>
                    <SelectContent>
                      {brokerConfig?.available_brokers.map((broker) => (
                        <SelectItem key={broker.id} value={broker.id}>
                          <div className="flex items-center gap-2">
                            <span>{broker.name}</span>
                            {broker.has_credentials && <Key className="h-3 w-3 text-green-500" />}
                          </div>
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                  {selectedBrokerInfo && !selectedBrokerInfo.has_credentials && (
                    <p className="text-xs text-muted-foreground text-center">
                      No credentials configured. Click Connect to add them.
                    </p>
                  )}
                  {selectedBrokerInfo && selectedBrokerInfo.has_credentials && (
                    <div className="flex items-center justify-center gap-2 mt-2">
                      <Button
                        type="button"
                        variant="outline"
                        size="sm"
                        onClick={handleEditCredentials}
                      >
                        <Edit2 className="h-3 w-3 mr-1" />
                        Edit
                      </Button>
                      <Button
                        type="button"
                        variant="outline"
                        size="sm"
                        className="text-destructive hover:text-destructive"
                        onClick={() => setShowDeleteDialog(true)}
                      >
                        <Trash2 className="h-3 w-3 mr-1" />
                        Delete
                      </Button>
                    </div>
                  )}
                </div>

                <Button type="submit" className="w-full" disabled={!selectedBroker || isSubmitting}>
                  {isSubmitting ? (
                    <>
                      <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                      Connecting...
                    </>
                  ) : selectedBrokerInfo?.has_credentials ? (
                    <>
                      <ExternalLink className="mr-2 h-4 w-4" />
                      Connect Account
                    </>
                  ) : (
                    <>
                      <Settings className="mr-2 h-4 w-4" />
                      Configure Credentials
                    </>
                  )}
                </Button>
              </form>
            </CardContent>
          </Card>

          {/* Left side content - Shown second on mobile */}
          <div className="flex-1 max-w-xl text-center lg:text-left order-2 lg:order-1">
            <h1 className="text-4xl lg:text-5xl font-bold mb-6">
              Connect Your <span className="text-primary">Broker</span>
            </h1>
            <p className="text-lg lg:text-xl mb-8 text-muted-foreground">
              Link your trading account to start executing trades through OpenAlgo's algorithmic
              trading platform.
            </p>

            <Alert className="mb-6">
              <Key className="h-4 w-4" />
              <AlertTitle>Secure Credentials</AlertTitle>
              <AlertDescription>
                Your API credentials are stored securely with AES-256 encryption.
              </AlertDescription>
            </Alert>

            <div className="flex justify-center lg:justify-start gap-4">
              <Button variant="outline" asChild>
                <a href="https://docs.openalgo.in" target="_blank" rel="noopener noreferrer">
                  <BookOpen className="mr-2 h-4 w-4" />
                  Documentation
                </a>
              </Button>
            </div>
          </div>
        </div>
      </div>

      {/* Credentials Entry Dialog */}
      <Dialog open={showCredentialsDialog} onOpenChange={(open) => {
        setShowCredentialsDialog(open)
        if (!open) {
          setIsEditMode(false)
          setCredentialsForm({ apiKey: '', apiSecret: '', clientId: '' })
        }
      }}>
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle>{isEditMode ? 'Update' : 'Configure'} {selectedBrokerInfo?.name} Credentials</DialogTitle>
            <DialogDescription>
              {isEditMode
                ? 'Enter your new broker API credentials. This will replace the existing credentials.'
                : 'Enter your broker API credentials. These will be stored securely with AES-256 encryption.'}
            </DialogDescription>
          </DialogHeader>
          <div className="space-y-4 py-4">
            {/* API Key - required for all brokers */}
            <div className="space-y-2">
              <Label htmlFor="apiKey">
                {selectedBroker === 'fyers' ? 'App ID *' : 'API Key *'}
              </Label>
              <Input
                id="apiKey"
                type="password"
                placeholder={selectedBroker === 'fyers' ? 'Enter your Fyers App ID (e.g., ABC123-100)' : 'Enter your API key'}
                value={credentialsForm.apiKey}
                onChange={(e) => setCredentialsForm({ ...credentialsForm, apiKey: e.target.value })}
              />
              {selectedBroker === 'fyers' && (
                <p className="text-xs text-muted-foreground">
                  Your Fyers App ID from the API dashboard (format: XXXXX-100)
                </p>
              )}
            </div>

            {/* API Secret - required for Fyers and Zerodha */}
            {(selectedBroker === 'fyers' || selectedBroker === 'zerodha') && (
              <div className="space-y-2">
                <Label htmlFor="apiSecret">
                  {selectedBroker === 'fyers' ? 'Secret Key *' : 'API Secret *'}
                </Label>
                <Input
                  id="apiSecret"
                  type="password"
                  placeholder={selectedBroker === 'fyers' ? 'Enter your Fyers Secret Key' : 'Enter your API secret'}
                  value={credentialsForm.apiSecret}
                  onChange={(e) =>
                    setCredentialsForm({ ...credentialsForm, apiSecret: e.target.value })
                  }
                />
              </div>
            )}

            {/* Client ID - required for Angel (not for Fyers) */}
            {selectedBroker === 'angel' && (
              <div className="space-y-2">
                <Label htmlFor="clientId">Client ID *</Label>
                <Input
                  id="clientId"
                  type="text"
                  placeholder="Enter your Angel One Client ID"
                  value={credentialsForm.clientId}
                  onChange={(e) =>
                    setCredentialsForm({ ...credentialsForm, clientId: e.target.value })
                  }
                />
              </div>
            )}

            {/* Redirect URL info for OAuth brokers */}
            {(selectedBroker === 'fyers' || selectedBroker === 'zerodha') && (
              <div className="space-y-2 p-3 bg-muted rounded-md">
                <Label className="text-sm font-medium">Redirect URL (set this in your broker API dashboard)</Label>
                <code className="block text-xs bg-background p-2 rounded border break-all">
                  {webhookConfig?.ngrok_url
                    ? `${webhookConfig.ngrok_url}/${selectedBroker}/callback`
                    : `http://${webhookConfig?.host || '127.0.0.1'}:${webhookConfig?.port || 5000}/${selectedBroker}/callback`}
                </code>
                <p className="text-xs text-muted-foreground">
                  Copy this URL to your {selectedBroker === 'fyers' ? 'Fyers' : 'Zerodha'} API app's redirect URL setting
                </p>
                {!webhookConfig?.enabled && (
                  <p className="text-xs text-destructive font-medium mt-2">
                    ⚠️ Warning: Webhook server is disabled. Enable it in Settings {'>'} Server Settings before using OAuth login.
                  </p>
                )}
              </div>
            )}
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setShowCredentialsDialog(false)}>
              Cancel
            </Button>
            <Button
              onClick={handleSaveCredentials}
              disabled={isSavingCredentials || !credentialsForm.apiKey}
            >
              {isSavingCredentials ? (
                <>
                  <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                  {isEditMode ? 'Updating...' : 'Saving...'}
                </>
              ) : (
                isEditMode ? 'Update Credentials' : 'Save Credentials'
              )}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* Delete Confirmation Dialog */}
      <AlertDialog open={showDeleteDialog} onOpenChange={setShowDeleteDialog}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete Broker Credentials?</AlertDialogTitle>
            <AlertDialogDescription>
              Are you sure you want to delete the credentials for {selectedBrokerInfo?.name}?
              This action cannot be undone. You will need to reconfigure the broker to connect again.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={isDeletingCredentials}>Cancel</AlertDialogCancel>
            <AlertDialogAction
              onClick={handleDeleteCredentials}
              disabled={isDeletingCredentials}
              className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
            >
              {isDeletingCredentials ? (
                <>
                  <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                  Deleting...
                </>
              ) : (
                'Delete'
              )}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  )
}
