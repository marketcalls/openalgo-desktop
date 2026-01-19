import { invoke } from '@tauri-apps/api/core'
import { BookOpen, ExternalLink, Key, Loader2, Settings } from 'lucide-react'
import { useEffect, useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { toast } from 'sonner'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
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
  const { user } = useAuthStore()
  const [selectedBroker, setSelectedBroker] = useState<string>('')
  const [isLoading, setIsLoading] = useState(true)
  const [isSubmitting, setIsSubmitting] = useState(false)
  const [isSavingCredentials, setIsSavingCredentials] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [brokerConfig, setBrokerConfig] = useState<BrokerConfigResponse | null>(null)
  const [showCredentialsDialog, setShowCredentialsDialog] = useState(false)
  const [credentialsForm, setCredentialsForm] = useState({
    apiKey: '',
    apiSecret: '',
    clientId: '',
  })

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
  }, [])

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

      toast.success('Broker credentials saved successfully')
      setShowCredentialsDialog(false)
      setCredentialsForm({ apiKey: '', apiSecret: '', clientId: '' })

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

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault()

    if (!selectedBroker) {
      setError('Please select a broker')
      return
    }

    // Check if broker has credentials
    const broker = brokerConfig?.available_brokers.find((b) => b.id === selectedBroker)
    if (!broker?.has_credentials) {
      setShowCredentialsDialog(true)
      return
    }

    setIsSubmitting(true)
    setError(null)

    try {
      // Get actual API key for OAuth URL generation
      const credsResponse = await invoke<{ broker_id: string; api_key_masked: string } | null>(
        'get_broker_credentials',
        { brokerId: selectedBroker }
      )

      // For TOTP brokers, navigate to TOTP form
      if (broker.auth_type === 'totp') {
        navigate(`/broker/${selectedBroker}/totp`)
        return
      }

      // For OAuth brokers, we need the actual API key
      // For now, redirect to credential entry since we can't get unmasked key
      // TODO: Implement proper OAuth flow with secure key handling
      setError('OAuth login requires entering credentials. Please configure your broker.')
      setShowCredentialsDialog(true)
    } catch (err) {
      console.error('Broker login error:', err)
      setError('Failed to initiate broker login')
    } finally {
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
                Your API credentials are stored securely in your system's keychain, not in files.
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
      <Dialog open={showCredentialsDialog} onOpenChange={setShowCredentialsDialog}>
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle>Configure {selectedBrokerInfo?.name} Credentials</DialogTitle>
            <DialogDescription>
              Enter your broker API credentials. These will be stored securely in your system's
              keychain.
            </DialogDescription>
          </DialogHeader>
          <div className="space-y-4 py-4">
            <div className="space-y-2">
              <Label htmlFor="apiKey">API Key *</Label>
              <Input
                id="apiKey"
                type="password"
                placeholder="Enter your API key"
                value={credentialsForm.apiKey}
                onChange={(e) => setCredentialsForm({ ...credentialsForm, apiKey: e.target.value })}
              />
            </div>
            <div className="space-y-2">
              <Label htmlFor="apiSecret">API Secret</Label>
              <Input
                id="apiSecret"
                type="password"
                placeholder="Enter your API secret (if required)"
                value={credentialsForm.apiSecret}
                onChange={(e) =>
                  setCredentialsForm({ ...credentialsForm, apiSecret: e.target.value })
                }
              />
            </div>
            <div className="space-y-2">
              <Label htmlFor="clientId">Client ID</Label>
              <Input
                id="clientId"
                type="text"
                placeholder="Enter your client ID (if required)"
                value={credentialsForm.clientId}
                onChange={(e) =>
                  setCredentialsForm({ ...credentialsForm, clientId: e.target.value })
                }
              />
            </div>
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
                  Saving...
                </>
              ) : (
                'Save Credentials'
              )}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  )
}
