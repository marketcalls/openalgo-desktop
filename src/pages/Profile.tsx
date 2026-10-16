import {
  AlertCircle,
  AlertTriangle,
  ArrowLeft,
  Bell,
  Check,
  Copy,
  Key,
  Lock,
  Moon,
  Palette,
  RefreshCw,
  RotateCcw,
  Shield,
  Sun,
  User,
  Volume2,
  VolumeX,
  X,
} from 'lucide-react'
import { useCallback, useEffect, useState } from 'react'
import { Link } from 'react-router'
import { type BrokerList, brokerConfigApi } from '@/api/broker-config'
import { webClient } from '@/api/client'
import TwoFactorEnforcement from '@/components/auth/TwoFactorEnforcement'
// Desktop: where the web's SMTP and System tabs are answered instead.
import { ProfileDesktopNote } from '@/components/desktop/ProfileDesktopNote'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@/components/ui/alert-dialog'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Checkbox } from '@/components/ui/checkbox'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Progress } from '@/components/ui/progress'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Switch } from '@/components/ui/switch'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { DESKTOP_BROKER_SETUP_PATH, desktopInitialProfileTab } from '@/lib/desktop'
import { type AlertCategories, type ToastPosition, useAlertStore } from '@/stores/alertStore'
import { useAuthStore } from '@/stores/authStore'
import { type ThemeColor, type ThemeMode, useThemeStore } from '@/stores/themeStore'
import { showToast, toast } from '@/utils/toast'

// Professional themes suitable for trading terminals
const THEME_MODES: { value: ThemeMode; label: string; icon: typeof Sun; description: string }[] = [
  {
    value: 'light',
    label: 'Light',
    icon: Sun,
    description: 'Clean, bright interface for daytime trading',
  },
  {
    value: 'dark',
    label: 'Dark',
    icon: Moon,
    description: 'Reduced eye strain for extended sessions',
  },
]

// Accent colors for customization
const ACCENT_COLORS: { value: ThemeColor; label: string; color: string }[] = [
  { value: 'zinc', label: 'Zinc', color: 'bg-zinc-500' },
  { value: 'slate', label: 'Slate', color: 'bg-slate-500' },
  { value: 'gray', label: 'Gray', color: 'bg-gray-500' },
  { value: 'neutral', label: 'Neutral', color: 'bg-neutral-500' },
  { value: 'green', label: 'Green', color: 'bg-green-500' },
  { value: 'blue', label: 'Blue', color: 'bg-blue-500' },
  { value: 'violet', label: 'Violet', color: 'bg-violet-500' },
  { value: 'orange', label: 'Orange', color: 'bg-orange-500' },
]

interface ProfileData {
  username: string
  smtp_settings: {
    smtp_server: string
    smtp_port: number
    smtp_username: string
    smtp_password: boolean
    smtp_use_tls: boolean
    smtp_from_email: string
    smtp_helo_hostname: string
  } | null
  qr_code: string | null
  totp_secret: string | null
}

interface PasswordRequirements {
  length: boolean
  uppercase: boolean
  lowercase: boolean
  number: boolean
  special: boolean
}

interface BrokerCredentials {
  broker_api_key: string
  broker_api_key_raw_length: number
  broker_api_secret: string
  broker_api_secret_raw_length: number
  broker_api_key_market: string
  broker_api_key_market_raw_length: number
  broker_api_secret_market: string
  broker_api_secret_market_raw_length: number
  redirect_url: string
  current_broker: string
  valid_brokers: string[]
  // Desktop: brokers whose sign-in needs a separate client id, and the saved one.
  client_id_brokers?: string[]
  client_id?: string | null
  ngrok_allow: boolean
  host_server: string
  websocket_url: string
  // Desktop: true while the address is the automatic one (nothing saved).
  host_server_is_default?: boolean
  websocket_url_is_default?: boolean
  server_status?: {
    flask: { host: string; port: string }
    websocket: { host: string; port: string }
    zmq: { host: string; port: string }
  }
}

// Toast position options
const TOAST_POSITIONS: { value: ToastPosition; label: string }[] = [
  { value: 'top-left', label: 'Top Left' },
  { value: 'top-center', label: 'Top Center' },
  { value: 'top-right', label: 'Top Right' },
  { value: 'bottom-left', label: 'Bottom Left' },
  { value: 'bottom-center', label: 'Bottom Center' },
  { value: 'bottom-right', label: 'Bottom Right' },
]

// Alert category descriptions - organized by groups
const ALERT_CATEGORIES_REALTIME: {
  key: keyof AlertCategories
  label: string
  description: string
}[] = [
  {
    key: 'orders',
    label: 'Order Notifications',
    description: 'Real-time BUY/SELL order placement, cancellation, and modification alerts',
  },
  {
    key: 'analyzer',
    label: 'Analyzer Mode',
    description: 'Real-time sandbox/analyzer mode operation notifications',
  },
  {
    key: 'system',
    label: 'System Alerts',
    description: 'Password changes, master contract downloads, and system events',
  },
  {
    key: 'actionCenter',
    label: 'Action Center',
    description: 'Pending order notifications in semi-auto mode',
  },
]

const ALERT_CATEGORIES_TRADING: {
  key: keyof AlertCategories
  label: string
  description: string
}[] = [
  {
    key: 'positions',
    label: 'Positions',
    description: 'Position close/update operations and P&L notifications',
  },
  {
    key: 'chartink',
    label: 'Chartink',
    description: 'Chartink strategy operations and scanner integrations',
  },
]

const ALERT_CATEGORIES_DATA: {
  key: keyof AlertCategories
  label: string
  description: string
}[] = [
  {
    key: 'historify',
    label: 'Historify',
    description: 'Historical data jobs, file uploads, downloads, and schedules',
  },
  // Desktop: Python Strategy and Flow alert categories removed (out of scope).
]

const ALERT_CATEGORIES_ADMIN: {
  key: keyof AlertCategories
  label: string
  description: string
}[] = [
  {
    key: 'telegram',
    label: 'Telegram Bot',
    description: 'Telegram bot operations, broadcasts, and user management',
  },
  {
    key: 'admin',
    label: 'Admin Panel',
    description: 'Market timings, holidays, freeze quantities, and admin settings',
  },
  {
    key: 'monitoring',
    label: 'Monitoring',
    description: 'Health monitor, latency dashboard, and security alerts',
  },
  {
    key: 'clipboard',
    label: 'Clipboard',
    description: 'Copy to clipboard confirmation messages',
  },
]

export default function ProfilePage() {
  const user = useAuthStore((s) => s.user)
  const { mode, color, appMode, setMode, setColor } = useThemeStore()
  const alertStore = useAlertStore()
  // Desktop: the broker page links here with ?tab=broker.
  const [activeTab, setActiveTab] = useState(() => desktopInitialProfileTab('account'))
  const [isLoading, setIsLoading] = useState(true)
  const [profileData, setProfileData] = useState<ProfileData | null>(null)

  // Password form state
  const [oldPassword, setOldPassword] = useState('')
  const [newPassword, setNewPassword] = useState('')
  const [confirmPassword, setConfirmPassword] = useState('')
  const [passwordRequirements, setPasswordRequirements] = useState<PasswordRequirements>({
    length: false,
    uppercase: false,
    lowercase: false,
    number: false,
    special: false,
  })
  const [isChangingPassword, setIsChangingPassword] = useState(false)

  // Broker credentials state
  const [brokerCredentials, setBrokerCredentials] = useState<BrokerCredentials | null>(null)
  // Desktop: the brokers with saved keys and the one logged in, for the
  // Broker tab's summary. Keys are edited on the Broker Configuration page.
  const [brokerList, setBrokerList] = useState<BrokerList | null>(null)
  const [brokerListError, setBrokerListError] = useState<string | null>(null)
  const [ngrokEnabled, setNgrokEnabled] = useState(false)
  const [hostServer, setHostServer] = useState('')
  const [websocketUrl, setWebsocketUrl] = useState('')
  const [isSavingNgrok, setIsSavingNgrok] = useState(false)
  const [showRestartDialog, setShowRestartDialog] = useState(false)

  // Check if in analyzer mode (theme changes blocked)
  const isAnalyzerMode = appMode === 'analyzer'

  // biome-ignore lint/correctness/useExhaustiveDependencies: one-time data load on mount; fetchProfileData/fetchBrokerCredentials are stable per-render fetchers and must not refire when their closures are recreated.
  useEffect(() => {
    fetchProfileData()
    fetchBrokerCredentials()
    fetchBrokerList()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  const fetchProfileData = async () => {
    try {
      const response = await webClient.get<{ status: string; data: ProfileData }>(
        '/auth/profile-data'
      )
      if (response.data.status === 'success') {
        setProfileData(response.data.data)
      }
    } catch (_error) {
      showToast.error('Failed to load profile data', 'admin')
    } finally {
      setIsLoading(false)
    }
  }

  const fetchBrokerCredentials = async () => {
    try {
      const response = await webClient.get<{ status: string; data: BrokerCredentials }>(
        '/api/broker/credentials'
      )
      if (response.data.status === 'success') {
        setBrokerCredentials(response.data.data)
        setNgrokEnabled(response.data.data.ngrok_allow)
        setHostServer(response.data.data.host_server)
        setWebsocketUrl(response.data.data.websocket_url || '')
      }
    } catch (_error) {}
  }

  const fetchBrokerList = async () => {
    try {
      setBrokerList(await brokerConfigApi.list())
      setBrokerListError(null)
    } catch (e) {
      setBrokerListError(e instanceof Error ? e.message : 'Could not load your brokers.')
    }
  }

  // Desktop: a URL is sent only when the trader edited it, so saving the
  // tunnel switch never stores the automatic addresses shown in the fields
  // as fixed ones. An emptied field goes back to the automatic address.
  const hostServerEdited = hostServer.trim() !== (brokerCredentials?.host_server ?? '')
  const websocketUrlEdited = websocketUrl.trim() !== (brokerCredentials?.websocket_url ?? '')

  const hasNgrokChanges = Boolean(
    ngrokEnabled !== brokerCredentials?.ngrok_allow || hostServerEdited || websocketUrlEdited
  )

  const handleNgrokSave = async () => {
    setIsSavingNgrok(true)
    try {
      // Send as JSON for more reliable handling
      const payload: {
        ngrok_allow: string
        host_server?: string
        websocket_url?: string
      } = {
        ngrok_allow: ngrokEnabled ? 'TRUE' : 'FALSE',
      }
      if (hostServerEdited) {
        payload.host_server = hostServer.trim()
      }
      if (websocketUrlEdited) {
        payload.websocket_url = websocketUrl.trim()
      }

      const response = await webClient.post<{
        status: string
        message: string
        restart_required?: boolean
      }>('/api/broker/credentials', payload)

      if (response.data.status === 'success') {
        showToast.success(response.data.message, 'admin')
        // Desktop: settings apply at once, so read them back: an emptied
        // field now shows the automatic address the server works out.
        await fetchBrokerCredentials()
        // Desktop: settings apply at once; ask for a restart only when the server says so.
        if (response.data.restart_required) setShowRestartDialog(true)
      } else {
        showToast.error(response.data.message || 'Failed to save settings', 'admin')
      }
    } catch (error: unknown) {
      const err = error as { response?: { data?: { message?: string } } }
      showToast.error(err.response?.data?.message || 'Failed to save settings', 'admin')
    } finally {
      setIsSavingNgrok(false)
    }
  }

  // Password validation
  const checkPasswordRequirements = useCallback((password: string) => {
    const requirements: PasswordRequirements = {
      length: password.length >= 8,
      uppercase: /[A-Z]/.test(password),
      lowercase: /[a-z]/.test(password),
      number: /[0-9]/.test(password),
      special: /[!@#$%^&*]/.test(password),
    }
    setPasswordRequirements(requirements)
    return Object.values(requirements).every(Boolean)
  }, [])

  useEffect(() => {
    checkPasswordRequirements(newPassword)
  }, [newPassword, checkPasswordRequirements])

  const getPasswordStrength = () => {
    const metCount = Object.values(passwordRequirements).filter(Boolean).length
    if (metCount === 0) return { percentage: 0, label: 'None', color: 'bg-gray-400' }
    if (metCount <= 2) return { percentage: 40, label: 'Weak', color: 'bg-red-500' }
    if (metCount <= 3) return { percentage: 60, label: 'Fair', color: 'bg-yellow-500' }
    if (metCount <= 4) return { percentage: 80, label: 'Good', color: 'bg-blue-500' }
    return { percentage: 100, label: 'Strong', color: 'bg-green-500' }
  }

  const passwordsMatch = newPassword === confirmPassword && confirmPassword !== ''
  const meetsAllRequirements = Object.values(passwordRequirements).every(Boolean)
  const canSubmitPassword = passwordsMatch && meetsAllRequirements && oldPassword !== ''

  const handleChangePassword = async (e: React.FormEvent) => {
    e.preventDefault()
    if (!canSubmitPassword) return

    setIsChangingPassword(true)
    try {
      const formData = new FormData()
      formData.append('old_password', oldPassword)
      formData.append('new_password', newPassword)
      formData.append('confirm_password', confirmPassword)

      const response = await webClient.post<{ status: string; message: string }>(
        '/auth/change-password',
        formData
      )

      if (response.data.status === 'success') {
        showToast.success(response.data.message || 'Password changed successfully', 'system')
        setOldPassword('')
        setNewPassword('')
        setConfirmPassword('')
      } else {
        showToast.error(response.data.message || 'Failed to change password', 'system')
      }
    } catch (error: unknown) {
      const err = error as { response?: { data?: { message?: string } } }
      showToast.error(err.response?.data?.message || 'Failed to change password', 'system')
    } finally {
      setIsChangingPassword(false)
    }
  }

  const handleThemeModeChange = (newMode: ThemeMode) => {
    if (isAnalyzerMode) {
      showToast.error('Cannot change theme while in Analyzer Mode', 'system')
      return
    }
    setMode(newMode)
    showToast.success(`Theme changed to ${newMode}`, 'system')
  }

  const handleAccentColorChange = (newColor: ThemeColor) => {
    if (isAnalyzerMode) {
      showToast.error('Cannot change theme while in Analyzer Mode', 'system')
      return
    }
    setColor(newColor)
    showToast.success(`Accent color changed to ${newColor}`, 'system')
  }

  const handleResetTheme = () => {
    if (isAnalyzerMode) {
      showToast.error('Cannot change theme while in Analyzer Mode', 'system')
      return
    }
    setMode('light')
    setColor('zinc')
    showToast.success('Theme reset to default', 'system')
  }

  const copyToClipboard = (text: string) => {
    navigator.clipboard
      .writeText(text)
      .then(() => {
        showToast.success('Copied to clipboard', 'clipboard')
      })
      .catch(() => {
        showToast.error('Failed to copy', 'clipboard')
      })
  }

  const strength = getPasswordStrength()

  if (isLoading) {
    return (
      <div className="flex items-center justify-center py-16">
        <div className="animate-spin rounded-full h-8 w-8 border-b-2 border-primary"></div>
      </div>
    )
  }

  return (
    <div className="py-6 max-w-3xl mx-auto space-y-6">
      {/* Header */}
      <div>
        <div className="flex items-center gap-2 mb-2">
          <Link to="/dashboard" className="text-muted-foreground hover:text-foreground">
            <ArrowLeft className="h-4 w-4" />
          </Link>
          <h1 className="text-2xl font-bold flex items-center gap-2">
            <User className="h-6 w-6" />
            Profile Settings
          </h1>
        </div>
        <p className="text-muted-foreground">
          Manage your account settings and security preferences
        </p>
      </div>

      {/* Tabs */}
      <Tabs value={activeTab} onValueChange={setActiveTab}>
        {/* Desktop: no SMTP or System tab (UI-05, UI-06; see ProfileDesktopNote). */}
        <TabsList className="grid w-full grid-cols-5">
          <TabsTrigger value="account" className="gap-1">
            <Lock className="h-4 w-4" />
            <span className="hidden sm:inline">Account</span>
          </TabsTrigger>
          <TabsTrigger value="broker" className="gap-1">
            <Key className="h-4 w-4" />
            <span className="hidden sm:inline">Broker</span>
          </TabsTrigger>
          <TabsTrigger value="alerts" className="gap-1">
            <Bell className="h-4 w-4" />
            <span className="hidden sm:inline">Alerts</span>
          </TabsTrigger>
          <TabsTrigger value="theme" className="gap-1">
            <Palette className="h-4 w-4" />
            <span className="hidden sm:inline">Theme</span>
          </TabsTrigger>
          <TabsTrigger value="totp" className="gap-1">
            <Shield className="h-4 w-4" />
            <span className="hidden sm:inline">TOTP</span>
          </TabsTrigger>
        </TabsList>

        {/* Account Tab */}
        <TabsContent value="account" className="space-y-6">
          {/* Account Info */}
          <Card>
            <CardHeader>
              <CardTitle>Account Information</CardTitle>
            </CardHeader>
            <CardContent className="grid grid-cols-2 gap-4">
              <div className="space-y-2">
                <Label>Username</Label>
                <Input value={user?.username || profileData?.username || ''} disabled />
              </div>
              <div className="space-y-2">
                <Label>Account Type</Label>
                <div className="pt-2">
                  <Badge>Administrator</Badge>
                </div>
              </div>
            </CardContent>
          </Card>

          {/* Password Change */}
          <Card>
            <CardHeader>
              <CardTitle>Change Password</CardTitle>
            </CardHeader>
            <CardContent>
              <form onSubmit={handleChangePassword} className="space-y-4">
                <div className="space-y-2">
                  <Label>Current Password</Label>
                  <Input
                    type="password"
                    placeholder="Enter your current password"
                    value={oldPassword}
                    onChange={(e) => setOldPassword(e.target.value)}
                  />
                </div>

                <div className="space-y-2">
                  <Label>New Password</Label>
                  <Input
                    type="password"
                    placeholder="Enter your new password"
                    value={newPassword}
                    onChange={(e) => setNewPassword(e.target.value)}
                  />
                </div>

                <div className="space-y-2">
                  <Label>Confirm New Password</Label>
                  <Input
                    type="password"
                    placeholder="Confirm your new password"
                    value={confirmPassword}
                    onChange={(e) => setConfirmPassword(e.target.value)}
                  />
                  {confirmPassword && (
                    <p className={`text-sm ${passwordsMatch ? 'text-green-500' : 'text-red-500'}`}>
                      {passwordsMatch ? 'Passwords match' : 'Passwords do not match'}
                    </p>
                  )}
                </div>

                {/* Password Strength */}
                <div className="bg-muted rounded-lg p-4 space-y-3">
                  <div className="flex justify-between text-sm">
                    <span>Strength</span>
                    <span className="font-medium">{strength.label}</span>
                  </div>
                  <Progress value={strength.percentage} className={strength.color} />

                  <div className="grid grid-cols-2 gap-2 text-xs">
                    {Object.entries(passwordRequirements).map(([key, met]) => (
                      <div
                        key={key}
                        className={`flex items-center gap-1 px-2 py-1 rounded-full border ${
                          met
                            ? 'bg-green-500/10 border-green-500 text-green-600'
                            : 'bg-muted border-border text-muted-foreground'
                        }`}
                      >
                        {met ? <Check className="h-3 w-3" /> : <X className="h-3 w-3" />}
                        <span>
                          {key === 'length' && '8+ chars'}
                          {key === 'uppercase' && 'A-Z'}
                          {key === 'lowercase' && 'a-z'}
                          {key === 'number' && '0-9'}
                          {key === 'special' && 'Special (@#$%^&*)'}
                        </span>
                      </div>
                    ))}
                  </div>
                </div>

                <Button type="submit" disabled={!canSubmitPassword || isChangingPassword}>
                  {isChangingPassword ? (
                    <>
                      <RefreshCw className="h-4 w-4 mr-2 animate-spin" />
                      Changing...
                    </>
                  ) : (
                    <>
                      <Lock className="h-4 w-4 mr-2" />
                      Change Password
                    </>
                  )}
                </Button>
              </form>
            </CardContent>
          </Card>
          <ProfileDesktopNote />
        </TabsContent>

        {/* Broker Tab */}
        <TabsContent value="broker" className="space-y-6">
          {/* Desktop: one place to edit broker keys (Broker Configuration);
              this tab shows a summary and links to it and to Select Broker. */}
          <Alert>
            <Key className="h-4 w-4" />
            <AlertTitle>Broker API Credentials</AlertTitle>
            <AlertDescription>
              Keys are saved per broker, for as many brokers as you like, and one broker is logged
              in at a time. Changes apply as soon as you save them on the Broker Configuration page.
            </AlertDescription>
          </Alert>

          <Card>
            <CardHeader>
              <CardTitle>Brokers</CardTitle>
              <CardDescription>
                The broker logged in now and the brokers with saved keys
              </CardDescription>
            </CardHeader>
            <CardContent className="space-y-4">
              {brokerListError && (
                <Alert variant="destructive">
                  <AlertDescription>{brokerListError}</AlertDescription>
                </Alert>
              )}
              <div className="grid grid-cols-1 gap-4 sm:grid-cols-2">
                <div className="space-y-2">
                  <Label>Logged in</Label>
                  <div data-testid="broker-logged-in">
                    {(() => {
                      const live = brokerList?.brokers.find((b) => b.active)
                      return live ? (
                        <Badge className="bg-green-600 text-white text-base">{live.name}</Badge>
                      ) : (
                        <span className="text-sm text-muted-foreground">No broker logged in</span>
                      )
                    })()}
                  </div>
                </div>
                <div className="space-y-2">
                  <Label>Configured brokers</Label>
                  <div className="flex flex-wrap gap-2" data-testid="broker-configured">
                    {(brokerList?.brokers ?? []).filter((b) => b.configured).length === 0 ? (
                      <span className="text-sm text-muted-foreground">None yet</span>
                    ) : (
                      (brokerList?.brokers ?? [])
                        .filter((b) => b.configured)
                        .sort((a, b) => a.name.localeCompare(b.name))
                        .map((b) => (
                          <Badge key={b.id} variant={b.active ? 'default' : 'secondary'}>
                            {b.name}
                          </Badge>
                        ))
                    )}
                  </div>
                </div>
              </div>
              <div className="flex flex-wrap gap-3 pt-2">
                <Button asChild>
                  <Link to="/broker">Select Broker</Link>
                </Button>
                <Button asChild variant="outline">
                  <Link to={DESKTOP_BROKER_SETUP_PATH}>Broker Configuration</Link>
                </Button>
              </div>
            </CardContent>
          </Card>

          {/* Server Status Card */}
          {brokerCredentials?.server_status && (
            <Card>
              <CardHeader>
                <CardTitle>Server Status</CardTitle>
                <CardDescription>Running services and their configured ports</CardDescription>
              </CardHeader>
              <CardContent>
                <div className="grid grid-cols-3 gap-4">
                  <div className="flex flex-col items-center p-3 bg-muted rounded-lg">
                    <Badge variant="default" className="mb-2 bg-green-600">
                      Running
                    </Badge>
                    <span className="text-sm font-medium">Flask App</span>
                    <span className="text-xs text-muted-foreground">
                      {brokerCredentials.server_status.flask.host}:
                      {brokerCredentials.server_status.flask.port}
                    </span>
                  </div>
                  <div className="flex flex-col items-center p-3 bg-muted rounded-lg">
                    <Badge variant="default" className="mb-2 bg-green-600">
                      Running
                    </Badge>
                    <span className="text-sm font-medium">WebSocket</span>
                    <span className="text-xs text-muted-foreground">
                      {brokerCredentials.server_status.websocket.host}:
                      {brokerCredentials.server_status.websocket.port}
                    </span>
                  </div>
                  <div className="flex flex-col items-center p-3 bg-muted rounded-lg">
                    <Badge variant="default" className="mb-2 bg-green-600">
                      Running
                    </Badge>
                    <span className="text-sm font-medium">ZeroMQ</span>
                    <span className="text-xs text-muted-foreground">
                      {brokerCredentials.server_status.zmq.host}:
                      {brokerCredentials.server_status.zmq.port}
                    </span>
                  </div>
                </div>
              </CardContent>
            </Card>
          )}

          {/* Ngrok Configuration - Separate Card */}
          <Card>
            <CardHeader>
              <CardTitle>Server Configuration</CardTitle>
              <CardDescription>
                Configure ngrok for receiving webhook alerts from external services like
                TradingView, Chartink, GoCharting, etc.
              </CardDescription>
            </CardHeader>
            <CardContent className="space-y-4">
              <div className="flex items-center space-x-2">
                <Checkbox
                  id="ngrok_enabled"
                  checked={ngrokEnabled}
                  onCheckedChange={(checked) => setNgrokEnabled(checked === true)}
                />
                <Label htmlFor="ngrok_enabled">Enable Ngrok Tunnel</Label>
                {ngrokEnabled !== brokerCredentials?.ngrok_allow && (
                  <Badge variant="outline" className="text-yellow-600">
                    Changed
                  </Badge>
                )}
              </div>

              <div className="space-y-2">
                <Label>Host Server URL</Label>
                <Input
                  value={hostServer}
                  onChange={(e) => setHostServer(e.target.value)}
                  placeholder="https://your-domain.ngrok-free.app"
                />
                <p className="text-xs text-muted-foreground">
                  Your ngrok domain or custom domain for receiving webhooks.
                </p>
                {hostServerEdited && hostServer.trim() && (
                  <Badge variant="outline" className="text-yellow-600">
                    Changed from: {brokerCredentials?.host_server}
                  </Badge>
                )}
                {hostServerEdited && !hostServer.trim() && (
                  <Badge variant="outline" className="text-yellow-600">
                    Uses the automatic address after saving
                  </Badge>
                )}
                {brokerCredentials?.host_server_is_default === false && hostServer.trim() && (
                  <Button
                    type="button"
                    variant="link"
                    size="sm"
                    className="h-auto p-0"
                    aria-label="Use the automatic host server address"
                    onClick={() => setHostServer('')}
                  >
                    Use automatic address
                  </Button>
                )}
              </div>

              <div className="space-y-2">
                <Label>WebSocket URL</Label>
                <Input
                  value={websocketUrl}
                  onChange={(e) => setWebsocketUrl(e.target.value)}
                  placeholder="ws://127.0.0.1:8765"
                />
                <p className="text-xs text-muted-foreground">
                  WebSocket server URL for real-time market data streaming.
                </p>
                {websocketUrlEdited && websocketUrl.trim() && (
                  <Badge variant="outline" className="text-yellow-600">
                    Changed from: {brokerCredentials?.websocket_url}
                  </Badge>
                )}
                {websocketUrlEdited && !websocketUrl.trim() && (
                  <Badge variant="outline" className="text-yellow-600">
                    Uses the automatic address after saving
                  </Badge>
                )}
                {brokerCredentials?.websocket_url_is_default === false && websocketUrl.trim() && (
                  <Button
                    type="button"
                    variant="link"
                    size="sm"
                    className="h-auto p-0"
                    aria-label="Use the automatic WebSocket address"
                    onClick={() => setWebsocketUrl('')}
                  >
                    Use automatic address
                  </Button>
                )}
              </div>

              <Button
                onClick={handleNgrokSave}
                disabled={!hasNgrokChanges || isSavingNgrok}
                className="w-full"
              >
                {isSavingNgrok ? (
                  <>
                    <RefreshCw className="h-4 w-4 mr-2 animate-spin" />
                    Saving...
                  </>
                ) : (
                  <>
                    <RefreshCw className="h-4 w-4 mr-2" />
                    Save Ngrok Settings
                  </>
                )}
              </Button>
            </CardContent>
          </Card>
        </TabsContent>

        {/* Alerts Tab */}
        <TabsContent value="alerts" className="space-y-6">
          <Alert>
            <Bell className="h-4 w-4" />
            <AlertTitle>Toast Notification Settings</AlertTitle>
            <AlertDescription>
              Configure how toast notifications appear. Useful for power users running multiple
              strategies who want to reduce notification spam.
            </AlertDescription>
          </Alert>

          {/* Master Controls */}
          <Card>
            <CardHeader>
              <CardTitle>Master Controls</CardTitle>
              <CardDescription>
                Enable or disable all toast notifications and sounds
              </CardDescription>
            </CardHeader>
            <CardContent className="space-y-6">
              {/* Toasts Enabled */}
              <div className="flex items-center justify-between">
                <div className="space-y-0.5">
                  <Label htmlFor="toasts-enabled">Toast Notifications</Label>
                  <p className="text-sm text-muted-foreground">
                    Show popup notifications for events
                  </p>
                </div>
                <Switch
                  id="toasts-enabled"
                  checked={alertStore.toastsEnabled}
                  onCheckedChange={alertStore.setToastsEnabled}
                />
              </div>

              {/* Sound Enabled */}
              <div className="flex items-center justify-between">
                <div className="space-y-0.5">
                  <Label htmlFor="sound-enabled" className="flex items-center gap-2">
                    {alertStore.soundEnabled ? (
                      <Volume2 className="h-4 w-4" />
                    ) : (
                      <VolumeX className="h-4 w-4" />
                    )}
                    Sound Alerts
                  </Label>
                  <p className="text-sm text-muted-foreground">
                    Play audio alert for important events
                  </p>
                </div>
                <Switch
                  id="sound-enabled"
                  checked={alertStore.soundEnabled}
                  onCheckedChange={alertStore.setSoundEnabled}
                />
              </div>
            </CardContent>
          </Card>

          {/* Display Settings */}
          <Card className={!alertStore.toastsEnabled ? 'opacity-60' : ''}>
            <CardHeader>
              <CardTitle>Display Settings</CardTitle>
              <CardDescription>Customize toast appearance and behavior</CardDescription>
            </CardHeader>
            <CardContent className="space-y-6">
              {/* Position */}
              <div className="space-y-2">
                <Label>Toast Position</Label>
                <Select
                  value={alertStore.position}
                  onValueChange={(value) => alertStore.setPosition(value as ToastPosition)}
                  disabled={!alertStore.toastsEnabled}
                >
                  <SelectTrigger>
                    <SelectValue placeholder="Select position" />
                  </SelectTrigger>
                  <SelectContent>
                    {TOAST_POSITIONS.map((pos) => (
                      <SelectItem key={pos.value} value={pos.value}>
                        {pos.label}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>

              {/* Max Visible Toasts */}
              <div className="space-y-2">
                <Label>Maximum Visible Toasts</Label>
                <div className="flex items-center gap-4">
                  <Input
                    type="number"
                    min={1}
                    max={10}
                    value={alertStore.maxVisibleToasts}
                    onChange={(e) => alertStore.setMaxVisibleToasts(Number(e.target.value))}
                    disabled={!alertStore.toastsEnabled}
                    className="w-24"
                  />
                  <span className="text-sm text-muted-foreground">
                    (1-10) Additional toasts will be queued
                  </span>
                </div>
              </div>

              {/* Duration */}
              <div className="space-y-2">
                <Label>Auto-dismiss Duration</Label>
                <div className="flex items-center gap-4">
                  <Input
                    type="number"
                    min={1}
                    max={15}
                    value={alertStore.duration / 1000}
                    onChange={(e) => alertStore.setDuration(Number(e.target.value) * 1000)}
                    disabled={!alertStore.toastsEnabled}
                    className="w-24"
                  />
                  <span className="text-sm text-muted-foreground">seconds (1-15)</span>
                </div>
              </div>
            </CardContent>
          </Card>

          {/* Category Controls - Real-time (Socket.IO) */}
          <Card className={!alertStore.toastsEnabled ? 'opacity-60' : ''}>
            <CardHeader>
              <CardTitle>Real-time Notifications</CardTitle>
              <CardDescription>
                High-frequency alerts from Socket.IO events. Disable these to reduce notification
                spam during active trading.
              </CardDescription>
            </CardHeader>
            <CardContent className="space-y-4">
              {ALERT_CATEGORIES_REALTIME.map((category) => (
                <div
                  key={category.key}
                  className="flex items-center justify-between py-2 border-b last:border-0"
                >
                  <div className="space-y-0.5">
                    <Label htmlFor={`category-${category.key}`}>{category.label}</Label>
                    <p className="text-sm text-muted-foreground">{category.description}</p>
                  </div>
                  <Switch
                    id={`category-${category.key}`}
                    checked={alertStore.categories[category.key]}
                    onCheckedChange={(checked) =>
                      alertStore.setCategoryEnabled(category.key, checked)
                    }
                    disabled={!alertStore.toastsEnabled}
                  />
                </div>
              ))}
            </CardContent>
          </Card>

          {/* Category Controls - Trading */}
          <Card className={!alertStore.toastsEnabled ? 'opacity-60' : ''}>
            <CardHeader>
              <CardTitle>Trading Operations</CardTitle>
              <CardDescription>
                Notifications from position management, strategies, and Chartink integrations
              </CardDescription>
            </CardHeader>
            <CardContent className="space-y-4">
              {ALERT_CATEGORIES_TRADING.map((category) => (
                <div
                  key={category.key}
                  className="flex items-center justify-between py-2 border-b last:border-0"
                >
                  <div className="space-y-0.5">
                    <Label htmlFor={`category-${category.key}`}>{category.label}</Label>
                    <p className="text-sm text-muted-foreground">{category.description}</p>
                  </div>
                  <Switch
                    id={`category-${category.key}`}
                    checked={alertStore.categories[category.key]}
                    onCheckedChange={(checked) =>
                      alertStore.setCategoryEnabled(category.key, checked)
                    }
                    disabled={!alertStore.toastsEnabled}
                  />
                </div>
              ))}
            </CardContent>
          </Card>

          {/* Category Controls - Data & Automation */}
          <Card className={!alertStore.toastsEnabled ? 'opacity-60' : ''}>
            <CardHeader>
              <CardTitle>Data & Automation</CardTitle>
              <CardDescription>Notifications from historical data jobs</CardDescription>
            </CardHeader>
            <CardContent className="space-y-4">
              {ALERT_CATEGORIES_DATA.map((category) => (
                <div
                  key={category.key}
                  className="flex items-center justify-between py-2 border-b last:border-0"
                >
                  <div className="space-y-0.5">
                    <Label htmlFor={`category-${category.key}`}>{category.label}</Label>
                    <p className="text-sm text-muted-foreground">{category.description}</p>
                  </div>
                  <Switch
                    id={`category-${category.key}`}
                    checked={alertStore.categories[category.key]}
                    onCheckedChange={(checked) =>
                      alertStore.setCategoryEnabled(category.key, checked)
                    }
                    disabled={!alertStore.toastsEnabled}
                  />
                </div>
              ))}
            </CardContent>
          </Card>

          {/* Category Controls - Admin & System */}
          <Card className={!alertStore.toastsEnabled ? 'opacity-60' : ''}>
            <CardHeader>
              <CardTitle>Admin & Utilities</CardTitle>
              <CardDescription>
                Notifications from admin panel, Telegram bot, monitoring, and clipboard operations
              </CardDescription>
            </CardHeader>
            <CardContent className="space-y-4">
              {ALERT_CATEGORIES_ADMIN.map((category) => (
                <div
                  key={category.key}
                  className="flex items-center justify-between py-2 border-b last:border-0"
                >
                  <div className="space-y-0.5">
                    <Label htmlFor={`category-${category.key}`}>{category.label}</Label>
                    <p className="text-sm text-muted-foreground">{category.description}</p>
                  </div>
                  <Switch
                    id={`category-${category.key}`}
                    checked={alertStore.categories[category.key]}
                    onCheckedChange={(checked) =>
                      alertStore.setCategoryEnabled(category.key, checked)
                    }
                    disabled={!alertStore.toastsEnabled}
                  />
                </div>
              ))}
            </CardContent>
          </Card>

          {/* Actions */}
          <Card>
            <CardHeader>
              <CardTitle>Actions</CardTitle>
              <CardDescription>Test and manage notifications</CardDescription>
            </CardHeader>
            <CardContent className="space-y-4">
              <div className="flex flex-wrap gap-3">
                <Button
                  variant="outline"
                  onClick={() => {
                    toast.success('This is a test success notification')
                  }}
                  disabled={!alertStore.toastsEnabled}
                >
                  Test Toast
                </Button>
                <Button
                  variant="outline"
                  onClick={() => {
                    toast.dismiss()
                    toast.success('All notifications cleared')
                  }}
                >
                  Clear All Toasts
                </Button>
                <Button
                  variant="outline"
                  onClick={() => {
                    alertStore.resetToDefaults()
                    toast.success('Settings reset to defaults')
                  }}
                >
                  <RotateCcw className="h-4 w-4 mr-2" />
                  Reset to Defaults
                </Button>
              </div>

              {/* Current Settings Summary */}
              <div className="mt-4 p-4 bg-muted rounded-lg">
                <h4 className="font-medium mb-2">Current Configuration</h4>
                <div className="grid grid-cols-2 gap-2 text-sm">
                  <span className="text-muted-foreground">Toasts:</span>
                  <span>{alertStore.toastsEnabled ? 'Enabled' : 'Disabled'}</span>
                  <span className="text-muted-foreground">Sound:</span>
                  <span>{alertStore.soundEnabled ? 'Enabled' : 'Disabled'}</span>
                  <span className="text-muted-foreground">Position:</span>
                  <span className="capitalize">{alertStore.position.replace('-', ' ')}</span>
                  <span className="text-muted-foreground">Max Visible:</span>
                  <span>{alertStore.maxVisibleToasts} toasts</span>
                  <span className="text-muted-foreground">Duration:</span>
                  <span>{alertStore.duration / 1000} seconds</span>
                </div>
              </div>
            </CardContent>
          </Card>

          {/* Help Card */}
          <Card>
            <CardHeader>
              <CardTitle>Understanding Notification Categories</CardTitle>
            </CardHeader>
            <CardContent className="text-sm text-muted-foreground space-y-4">
              <div>
                <p className="font-medium text-foreground mb-2">
                  Real-time Notifications (High Priority)
                </p>
                <ul className="list-disc list-inside space-y-1 ml-2">
                  <li>
                    <strong>Orders:</strong> BUY/SELL alerts from Socket.IO - highest frequency
                    during trading
                  </li>
                  <li>
                    <strong>Analyzer:</strong> Sandbox mode operations - can spam during sandbox
                    trading
                  </li>
                  <li>
                    <strong>System:</strong> Password changes, master contracts - infrequent but
                    important
                  </li>
                  <li>
                    <strong>Action Center:</strong> Semi-auto pending orders - high frequency in
                    managed accounts
                  </li>
                </ul>
              </div>
              <div>
                <p className="font-medium text-foreground mb-2">Trading Operations</p>
                <ul className="list-disc list-inside space-y-1 ml-2">
                  <li>
                    <strong>Positions:</strong> Position close/update notifications
                  </li>
                  <li>
                    <strong>Chartink:</strong> Chartink scanner and strategy integrations
                  </li>
                </ul>
              </div>
              <div>
                <p className="font-medium text-foreground mb-2">Data & Automation</p>
                <ul className="list-disc list-inside space-y-1 ml-2">
                  <li>
                    <strong>Historify:</strong> Historical data jobs, uploads, downloads (67 toasts
                    - highest volume)
                  </li>
                </ul>
              </div>
              <div>
                <p className="font-medium text-foreground mb-2">Admin & Utilities</p>
                <ul className="list-disc list-inside space-y-1 ml-2">
                  <li>
                    <strong>Telegram:</strong> Bot operations, broadcasts, user management
                  </li>
                  <li>
                    <strong>Admin:</strong> Market timings, holidays, freeze quantities
                  </li>
                  <li>
                    <strong>Monitoring:</strong> Health, latency, and security dashboards
                  </li>
                  <li>
                    <strong>Clipboard:</strong> Copy-to-clipboard confirmations
                  </li>
                </ul>
              </div>
              <div className="pt-3 border-t">
                <p className="font-medium text-foreground mb-2">Tips for Power Users</p>
                <ul className="list-disc list-inside space-y-1 ml-2">
                  <li>
                    Running 5+ strategies? Set max visible to <strong>2-3</strong> and duration to{' '}
                    <strong>2 seconds</strong>
                  </li>
                  <li>
                    Disable <strong>Analyzer</strong> when not sandbox trading to reduce spam
                  </li>
                  <li>
                    Disable <strong>Orders</strong> if you prefer checking the order book manually
                  </li>
                  <li>
                    Disable <strong>Historify</strong> during large data imports to avoid
                    notification flood
                  </li>
                  <li>
                    Keep <strong>System</strong> enabled for critical security alerts
                  </li>
                </ul>
              </div>
            </CardContent>
          </Card>
        </TabsContent>

        {/* Theme Tab */}
        <TabsContent value="theme" className="space-y-6">
          {/* Analyzer Mode Warning */}
          {isAnalyzerMode && (
            <Alert variant="destructive">
              <AlertCircle className="h-4 w-4" />
              <AlertTitle>Theme Changes Disabled</AlertTitle>
              <AlertDescription>
                Theme customization is disabled while in Analyzer Mode. The purple theme is
                automatically applied for visual distinction. Switch back to Live Mode to change
                themes.
              </AlertDescription>
            </Alert>
          )}

          {/* Current Theme Status */}
          <Card>
            <CardHeader>
              <div className="flex items-center justify-between">
                <div>
                  <CardTitle>Current Theme</CardTitle>
                  <CardDescription>
                    {isAnalyzerMode
                      ? 'Analyzer Mode (Purple Theme)'
                      : `${mode.charAt(0).toUpperCase() + mode.slice(1)} Mode with ${color.charAt(0).toUpperCase() + color.slice(1)} accent`}
                  </CardDescription>
                </div>
                <div className="flex items-center gap-4">
                  <div className="flex gap-2">
                    <div className="w-6 h-6 rounded bg-primary" title="Primary" />
                    <div className="w-6 h-6 rounded bg-secondary" title="Secondary" />
                    <div className="w-6 h-6 rounded bg-accent" title="Accent" />
                    <div className="w-6 h-6 rounded bg-muted" title="Muted" />
                  </div>
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={handleResetTheme}
                    disabled={isAnalyzerMode || (mode === 'light' && color === 'zinc')}
                  >
                    <RefreshCw className="h-4 w-4 mr-2" />
                    Reset
                  </Button>
                </div>
              </div>
            </CardHeader>
          </Card>

          {/* Theme Mode Selection */}
          <Card className={isAnalyzerMode ? 'opacity-60' : ''}>
            <CardHeader>
              <CardTitle>Theme Mode</CardTitle>
              <CardDescription>Choose between light and dark interface</CardDescription>
            </CardHeader>
            <CardContent>
              <div className="grid grid-cols-1 sm:grid-cols-2 gap-4">
                {THEME_MODES.map((themeOption) => {
                  const Icon = themeOption.icon
                  const isSelected = mode === themeOption.value && !isAnalyzerMode
                  return (
                    <button
                      type="button"
                      key={themeOption.value}
                      onClick={() => handleThemeModeChange(themeOption.value)}
                      disabled={isAnalyzerMode}
                      className={`flex items-start gap-4 p-4 rounded-lg border-2 transition-all text-left ${
                        isSelected
                          ? 'border-primary bg-primary/5'
                          : 'border-border hover:border-primary/50'
                      } ${isAnalyzerMode ? 'cursor-not-allowed' : 'cursor-pointer'}`}
                    >
                      <div
                        className={`p-2 rounded-lg ${isSelected ? 'bg-primary text-primary-foreground' : 'bg-muted'}`}
                      >
                        <Icon className="h-5 w-5" />
                      </div>
                      <div className="flex-1">
                        <div className="flex items-center gap-2">
                          <span className="font-medium">{themeOption.label}</span>
                          {isSelected && <Check className="h-4 w-4 text-primary" />}
                        </div>
                        <p className="text-sm text-muted-foreground mt-1">
                          {themeOption.description}
                        </p>
                      </div>
                    </button>
                  )
                })}
              </div>
            </CardContent>
          </Card>

          {/* Accent Color Selection */}
          <Card className={isAnalyzerMode ? 'opacity-60' : ''}>
            <CardHeader>
              <CardTitle>Accent Color</CardTitle>
              <CardDescription>Customize the primary accent color</CardDescription>
            </CardHeader>
            <CardContent>
              <div className="grid grid-cols-4 sm:grid-cols-8 gap-3">
                {ACCENT_COLORS.map((accentColor) => {
                  const isSelected = color === accentColor.value && !isAnalyzerMode
                  return (
                    <button
                      type="button"
                      key={accentColor.value}
                      onClick={() => handleAccentColorChange(accentColor.value)}
                      disabled={isAnalyzerMode}
                      className={`flex flex-col items-center gap-2 p-3 rounded-lg border-2 transition-all ${
                        isSelected
                          ? 'border-primary bg-primary/5'
                          : 'border-border hover:border-primary/50'
                      } ${isAnalyzerMode ? 'cursor-not-allowed' : 'cursor-pointer'}`}
                      title={accentColor.label}
                    >
                      <div
                        className={`w-8 h-8 rounded-full ${accentColor.color} ring-2 ring-offset-2 ring-offset-background ${isSelected ? 'ring-primary' : 'ring-transparent'}`}
                      />
                      <span className="text-xs font-medium">{accentColor.label}</span>
                    </button>
                  )
                })}
              </div>
            </CardContent>
          </Card>

          {/* Theme Info */}
          <Card>
            <CardHeader>
              <CardTitle>About Themes</CardTitle>
            </CardHeader>
            <CardContent className="text-sm text-muted-foreground space-y-2">
              <p>
                <strong>Light Mode:</strong> Optimized for daytime use with high contrast and
                readability.
              </p>
              <p>
                <strong>Dark Mode:</strong> Reduces eye strain during extended trading sessions,
                especially in low-light environments.
              </p>
              <p>
                <strong>Analyzer Mode:</strong> When in sandbox/analyzer mode, a distinct purple
                theme is applied automatically to clearly indicate you are not trading with real
                funds.
              </p>
            </CardContent>
          </Card>
        </TabsContent>

        {/* TOTP Tab */}
        <TabsContent value="totp" className="space-y-6">
          <Card>
            <CardHeader>
              <CardTitle>TOTP Authentication</CardTitle>
              <CardDescription>Manage your Two-Factor Authentication settings</CardDescription>
            </CardHeader>
            <CardContent className="space-y-6">
              <Alert>
                <Shield className="h-4 w-4" />
                <AlertTitle>About TOTP Authentication</AlertTitle>
                <AlertDescription>
                  TOTP (Time-based One-Time Password) provides an additional layer of security.
                  You&apos;ll need an authenticator app like Google Authenticator or Authy to
                  generate codes for password recovery.
                </AlertDescription>
              </Alert>

              <TwoFactorEnforcement />

              {profileData?.qr_code && profileData?.totp_secret ? (
                <div className="bg-muted rounded-lg p-6 space-y-6">
                  <h3 className="font-semibold text-center">Your TOTP QR Code</h3>

                  {/* QR Code */}
                  <div className="flex justify-center">
                    <div className="p-4 bg-white rounded-lg shadow">
                      <img
                        src={`data:image/png;base64,${profileData.qr_code}`}
                        alt="TOTP QR Code"
                        className="w-48 h-48"
                      />
                    </div>
                  </div>

                  {/* Manual Entry */}
                  <div className="bg-background rounded-lg p-4 space-y-2">
                    <h4 className="font-semibold">Manual Entry</h4>
                    <p className="text-sm text-muted-foreground">Secret key for manual entry:</p>
                    <div className="flex items-center gap-2">
                      <code className="flex-1 bg-muted p-2 rounded text-sm break-all">
                        {profileData.totp_secret}
                      </code>
                      <Button
                        size="icon"
                        variant="outline"
                        onClick={() => copyToClipboard(profileData.totp_secret!)}
                        aria-label="Copy TOTP secret"
                      >
                        <Copy className="h-4 w-4" />
                      </Button>
                    </div>
                  </div>

                  {/* Instructions */}
                  <div className="space-y-2">
                    <p className="font-medium">Setup Instructions:</p>
                    <ol className="list-decimal list-inside space-y-1 text-sm text-muted-foreground">
                      <li>Install an authenticator app (Google Authenticator, Authy, etc.)</li>
                      <li>Scan the QR code above or enter the secret key manually</li>
                      <li>Save your backup codes in a safe place</li>
                      <li>
                        You&apos;ll need the TOTP code to reset your password if you forget it
                      </li>
                    </ol>
                  </div>
                </div>
              ) : (
                <Alert variant="destructive">
                  <AlertTitle>TOTP Not Available</AlertTitle>
                  <AlertDescription>
                    Unable to load TOTP setup. Please contact your administrator.
                  </AlertDescription>
                </Alert>
              )}
            </CardContent>
          </Card>
        </TabsContent>
      </Tabs>

      {/* Restart Required Dialog */}
      <AlertDialog open={showRestartDialog} onOpenChange={setShowRestartDialog}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle className="flex items-center gap-2">
              <AlertTriangle className="h-5 w-5 text-yellow-500" />
              Restart Required
            </AlertDialogTitle>
            <AlertDialogDescription className="space-y-3">
              {/* Desktop: settings are stored in-app; a restart means quitting the app. */}
              <p>Your configuration has been saved.</p>
              <p>To apply these changes, quit OpenAlgo Desktop and open it again.</p>
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogAction onClick={() => setShowRestartDialog(false)}>
              Got it
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  )
}
