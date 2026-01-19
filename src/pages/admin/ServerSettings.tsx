import { invoke } from '@tauri-apps/api/core'
import { ArrowLeft, Globe, Loader2, RefreshCw, Save, Server } from 'lucide-react'
import { useEffect, useState } from 'react'
import { Link } from 'react-router-dom'
import { toast } from 'sonner'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Switch } from '@/components/ui/switch'

interface WebhookConfig {
  enabled: boolean
  host: string
  port: number
  ngrok_url: string | null
  webhook_secret: string | null
}

export default function ServerSettings() {
  const [isLoading, setIsLoading] = useState(true)
  const [isSaving, setIsSaving] = useState(false)
  const [config, setConfig] = useState<WebhookConfig>({
    enabled: false,
    host: '127.0.0.1',
    port: 5000,
    ngrok_url: null,
    webhook_secret: null,
  })

  const fetchConfig = async () => {
    try {
      setIsLoading(true)
      const data = await invoke<WebhookConfig>('get_webhook_config')
      setConfig(data)
    } catch (error) {
      console.error('Failed to load webhook config:', error)
      toast.error('Failed to load server configuration')
    } finally {
      setIsLoading(false)
    }
  }

  useEffect(() => {
    fetchConfig()
  }, [])

  const handleSave = async () => {
    try {
      setIsSaving(true)
      await invoke('update_webhook_config', {
        request: {
          enabled: config.enabled,
          host: config.host,
          port: config.port,
          ngrok_url: config.ngrok_url || null,
          webhook_secret: config.webhook_secret || null,
        },
      })
      toast.success('Server configuration saved. Restart app to apply changes.')
    } catch (error) {
      console.error('Failed to save webhook config:', error)
      toast.error('Failed to save server configuration')
    } finally {
      setIsSaving(false)
    }
  }

  // Generate redirect URL based on config
  const getRedirectUrl = () => {
    if (config.ngrok_url) {
      return `${config.ngrok_url}/:broker/callback`
    }
    return `http://${config.host}:${config.port}/:broker/callback`
  }

  // Generate webhook URL based on config
  const getWebhookUrl = () => {
    if (config.ngrok_url) {
      return `${config.ngrok_url}/webhook/{webhook_id}`
    }
    return `http://${config.host}:${config.port}/webhook/{webhook_id}`
  }

  // Generate REST API URL based on config
  const getApiUrl = () => {
    if (config.ngrok_url) {
      return `${config.ngrok_url}/api/v1/`
    }
    return `http://${config.host}:${config.port}/api/v1/`
  }

  if (isLoading) {
    return (
      <div className="flex items-center justify-center py-16">
        <Loader2 className="h-8 w-8 animate-spin" />
      </div>
    )
  }

  return (
    <div className="py-6 space-y-6">
      {/* Header */}
      <div className="flex items-center gap-4">
        <Link to="/admin">
          <Button variant="ghost" size="icon">
            <ArrowLeft className="h-5 w-5" />
          </Button>
        </Link>
        <div>
          <h1 className="text-2xl font-bold flex items-center gap-2">
            <Server className="h-6 w-6" />
            Server Settings
          </h1>
          <p className="text-muted-foreground mt-1">
            Configure webhook server, REST API, and OAuth redirect URLs
          </p>
        </div>
      </div>

      {/* Server Configuration */}
      <Card>
        <CardHeader>
          <CardTitle>API Server Configuration</CardTitle>
          <CardDescription>
            Configure the local HTTP server for webhooks and REST API
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-6">
          {/* Enable/Disable */}
          <div className="flex items-center justify-between">
            <div className="space-y-0.5">
              <Label htmlFor="server-enabled">Enable API Server</Label>
              <p className="text-sm text-muted-foreground">
                Start the HTTP server for webhooks and REST API
              </p>
            </div>
            <Switch
              id="server-enabled"
              checked={config.enabled}
              onCheckedChange={(enabled) => setConfig({ ...config, enabled })}
            />
          </div>

          {/* Host */}
          <div className="space-y-2">
            <Label htmlFor="host">Host Address</Label>
            <Input
              id="host"
              placeholder="127.0.0.1"
              value={config.host}
              onChange={(e) => setConfig({ ...config, host: e.target.value })}
            />
            <p className="text-xs text-muted-foreground">
              Use 127.0.0.1 for local only, 0.0.0.0 to accept from any interface
            </p>
          </div>

          {/* Port */}
          <div className="space-y-2">
            <Label htmlFor="port">Port</Label>
            <Input
              id="port"
              type="number"
              placeholder="5000"
              value={config.port}
              onChange={(e) => setConfig({ ...config, port: parseInt(e.target.value) || 5000 })}
            />
            <p className="text-xs text-muted-foreground">
              Port number for the API server (default: 5000)
            </p>
          </div>

          {/* Ngrok URL */}
          <div className="space-y-2">
            <Label htmlFor="ngrok-url">Ngrok/Public URL (Optional)</Label>
            <Input
              id="ngrok-url"
              placeholder="https://your-domain.ngrok-free.app"
              value={config.ngrok_url || ''}
              onChange={(e) => setConfig({ ...config, ngrok_url: e.target.value || null })}
            />
            <p className="text-xs text-muted-foreground">
              Public URL if using ngrok or a reverse proxy. Used for OAuth redirects and webhooks.
            </p>
          </div>

          {/* Webhook Secret */}
          <div className="space-y-2">
            <Label htmlFor="webhook-secret">Webhook Secret (Optional)</Label>
            <Input
              id="webhook-secret"
              type="password"
              placeholder="Optional secret for webhook validation"
              value={config.webhook_secret || ''}
              onChange={(e) => setConfig({ ...config, webhook_secret: e.target.value || null })}
            />
            <p className="text-xs text-muted-foreground">
              Secret key for validating incoming webhooks (optional)
            </p>
          </div>
        </CardContent>
      </Card>

      {/* URL Information */}
      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <Globe className="h-5 w-5" />
            Generated URLs
          </CardTitle>
          <CardDescription>
            Use these URLs in your broker configuration and trading platforms
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
          {/* OAuth Redirect URL */}
          <div className="space-y-2">
            <Label>OAuth Redirect URL (for Fyers, Zerodha)</Label>
            <div className="flex gap-2">
              <Input readOnly value={getRedirectUrl()} className="font-mono text-sm" />
              <Button
                variant="outline"
                size="icon"
                onClick={() => {
                  navigator.clipboard.writeText(getRedirectUrl())
                  toast.success('Copied to clipboard')
                }}
              >
                <RefreshCw className="h-4 w-4" />
              </Button>
            </div>
            <p className="text-xs text-muted-foreground">
              Replace :broker with the broker name (e.g., fyers, zerodha)
            </p>
          </div>

          {/* Webhook URL */}
          <div className="space-y-2">
            <Label>Webhook URL (for TradingView, GoCharting)</Label>
            <div className="flex gap-2">
              <Input readOnly value={getWebhookUrl()} className="font-mono text-sm" />
              <Button
                variant="outline"
                size="icon"
                onClick={() => {
                  navigator.clipboard.writeText(getWebhookUrl())
                  toast.success('Copied to clipboard')
                }}
              >
                <RefreshCw className="h-4 w-4" />
              </Button>
            </div>
            <p className="text-xs text-muted-foreground">
              Replace {'{webhook_id}'} with your strategy's webhook ID
            </p>
          </div>

          {/* REST API URL */}
          <div className="space-y-2">
            <Label>REST API Base URL (for OpenAlgo SDK)</Label>
            <div className="flex gap-2">
              <Input readOnly value={getApiUrl()} className="font-mono text-sm" />
              <Button
                variant="outline"
                size="icon"
                onClick={() => {
                  navigator.clipboard.writeText(getApiUrl())
                  toast.success('Copied to clipboard')
                }}
              >
                <RefreshCw className="h-4 w-4" />
              </Button>
            </div>
          </div>
        </CardContent>
      </Card>

      {/* Info Alert */}
      <Alert>
        <Server className="h-4 w-4" />
        <AlertTitle>Server Restart Required</AlertTitle>
        <AlertDescription>
          Changes to server settings require restarting the application to take effect.
          The server will automatically start on app launch if enabled.
        </AlertDescription>
      </Alert>

      {/* Save Button */}
      <div className="flex justify-end">
        <Button onClick={handleSave} disabled={isSaving}>
          {isSaving ? (
            <>
              <Loader2 className="mr-2 h-4 w-4 animate-spin" />
              Saving...
            </>
          ) : (
            <>
              <Save className="mr-2 h-4 w-4" />
              Save Settings
            </>
          )}
        </Button>
      </div>
    </div>
  )
}
