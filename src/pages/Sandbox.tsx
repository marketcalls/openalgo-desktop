import { BarChart3, RotateCcw, Save, Settings } from 'lucide-react'
import { useEffect, useState } from 'react'
import { Link } from 'react-router-dom'
import { toast } from 'sonner'
import { sandboxCommands, type SandboxConfig } from '@/api/tauri-client'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
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

const DAYS_OF_WEEK = [
  'Never',
  'Monday',
  'Tuesday',
  'Wednesday',
  'Thursday',
  'Friday',
  'Saturday',
  'Sunday',
]

const CAPITAL_OPTIONS = [
  { value: '100000', label: '1,00,000 (1 Lakh)' },
  { value: '500000', label: '5,00,000 (5 Lakhs)' },
  { value: '1000000', label: '10,00,000 (10 Lakhs)' },
  { value: '2500000', label: '25,00,000 (25 Lakhs)' },
  { value: '5000000', label: '50,00,000 (50 Lakhs)' },
  { value: '10000000', label: '1,00,00,000 (1 Crore)' },
]

// Config key to display name mapping
const CONFIG_LABELS: Record<string, string> = {
  starting_capital: 'Starting Capital',
  reset_day: 'Reset Day',
  reset_time: 'Reset Time',
  order_check_interval: 'Order Check Interval (s)',
  mtm_update_interval: 'MTM Update Interval (s)',
  nse_mis_leverage: 'NSE MIS Leverage',
  nfo_mis_leverage: 'NFO MIS Leverage',
  cds_mis_leverage: 'CDS MIS Leverage',
  mcx_mis_leverage: 'MCX MIS Leverage',
  nse_cnc_leverage: 'NSE CNC Leverage',
  nfo_nrml_leverage: 'NFO NRML Leverage',
  cds_nrml_leverage: 'CDS NRML Leverage',
  mcx_nrml_leverage: 'MCX NRML Leverage',
  nse_square_off_time: 'NSE Square Off Time',
  nfo_square_off_time: 'NFO Square Off Time',
  cds_square_off_time: 'CDS Square Off Time',
  mcx_square_off_time: 'MCX Square Off Time',
}

// Config key to description mapping
const CONFIG_DESCRIPTIONS: Record<string, string> = {
  starting_capital: 'Initial capital for paper trading',
  reset_day: 'Day of week to reset sandbox data',
  reset_time: 'Time to reset sandbox data (HH:MM)',
  order_check_interval: 'Interval to check pending orders (seconds)',
  mtm_update_interval: 'Interval to update MTM (seconds, 0 to disable)',
  nse_mis_leverage: 'Leverage for NSE MIS orders',
  nfo_mis_leverage: 'Leverage for NFO MIS orders',
  cds_mis_leverage: 'Leverage for CDS MIS orders',
  mcx_mis_leverage: 'Leverage for MCX MIS orders',
  nse_cnc_leverage: 'Leverage for NSE CNC orders',
  nfo_nrml_leverage: 'Leverage for NFO NRML orders',
  cds_nrml_leverage: 'Leverage for CDS NRML orders',
  mcx_nrml_leverage: 'Leverage for MCX NRML orders',
  nse_square_off_time: 'Auto square off time for NSE',
  nfo_square_off_time: 'Auto square off time for NFO',
  cds_square_off_time: 'Auto square off time for CDS',
  mcx_square_off_time: 'Auto square off time for MCX',
}

// Group configs into categories
const CONFIG_CATEGORIES = {
  general: {
    title: 'General Settings',
    keys: ['starting_capital', 'reset_day', 'reset_time', 'order_check_interval', 'mtm_update_interval'],
  },
  leverage: {
    title: 'Leverage Settings',
    keys: [
      'nse_mis_leverage',
      'nfo_mis_leverage',
      'cds_mis_leverage',
      'mcx_mis_leverage',
      'nse_cnc_leverage',
      'nfo_nrml_leverage',
      'cds_nrml_leverage',
      'mcx_nrml_leverage',
    ],
  },
  squareOff: {
    title: 'Square Off Times',
    keys: ['nse_square_off_time', 'nfo_square_off_time', 'cds_square_off_time', 'mcx_square_off_time'],
  },
}

export default function Sandbox() {
  const [config, setConfig] = useState<SandboxConfig | null>(null)
  const [localConfig, setLocalConfig] = useState<Record<string, string>>({})
  const [modifiedKeys, setModifiedKeys] = useState<Set<string>>(new Set())
  const [isLoading, setIsLoading] = useState(true)
  const [isResetting, setIsResetting] = useState(false)
  const [showResetDialog, setShowResetDialog] = useState(false)

  // Fetch config on mount
  useEffect(() => {
    fetchConfig()
  }, [])

  const fetchConfig = async () => {
    try {
      const data = await sandboxCommands.getSandboxConfig()
      setConfig(data)
      // Convert config to local string values for editing
      const localValues: Record<string, string> = {}
      for (const [key, value] of Object.entries(data)) {
        localValues[key] = String(value)
      }
      setLocalConfig(localValues)
      setModifiedKeys(new Set())
    } catch (error) {
      console.error('Error fetching config:', error)
      toast.error('Failed to load configuration')
    } finally {
      setIsLoading(false)
    }
  }

  const updateLocalValue = (key: string, value: string) => {
    setLocalConfig((prev) => ({ ...prev, [key]: value }))
    setModifiedKeys((prev) => new Set(prev).add(key))
  }

  const saveConfig = async (key: string) => {
    const value = localConfig[key]
    if (!value) return

    try {
      await sandboxCommands.updateSandboxConfig(key, value)
      toast.success(`${CONFIG_LABELS[key]} updated`)
      setModifiedKeys((prev) => {
        const updated = new Set(prev)
        updated.delete(key)
        return updated
      })
    } catch (error) {
      console.error('Error saving config:', error)
      toast.error('Failed to save configuration')
    }
  }

  const resetConfiguration = async () => {
    setIsResetting(true)
    try {
      await sandboxCommands.resetSandbox()
      toast.success('Sandbox reset successfully')
      setShowResetDialog(false)
      // Reload config after reset
      setTimeout(fetchConfig, 500)
    } catch (error) {
      console.error('Error resetting sandbox:', error)
      toast.error('Failed to reset sandbox')
    } finally {
      setIsResetting(false)
    }
  }

  const renderConfigInput = (key: string) => {
    const value = localConfig[key] || ''
    const isModified = modifiedKeys.has(key)

    // Reset Day selector
    if (key === 'reset_day') {
      return (
        <div className="flex gap-2">
          <Select value={value} onValueChange={(v) => updateLocalValue(key, v)}>
            <SelectTrigger className="flex-1">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {DAYS_OF_WEEK.map((day) => (
                <SelectItem key={day} value={day}>
                  {day}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          <Button
            size="sm"
            variant={isModified ? 'default' : 'secondary'}
            onClick={() => saveConfig(key)}
          >
            <Save className="h-4 w-4 mr-1" />
            Set
          </Button>
        </div>
      )
    }

    // Time inputs
    if (key === 'reset_time' || key.endsWith('_square_off_time')) {
      return (
        <div className="flex gap-2">
          <Input
            type="time"
            value={value}
            onChange={(e) => updateLocalValue(key, e.target.value)}
            className="flex-1"
          />
          <Button
            size="sm"
            variant={isModified ? 'default' : 'secondary'}
            onClick={() => saveConfig(key)}
          >
            <Save className="h-4 w-4 mr-1" />
            Set
          </Button>
        </div>
      )
    }

    // Leverage inputs
    if (key.endsWith('_leverage')) {
      return (
        <div className="flex gap-2">
          <Input
            type="number"
            value={value}
            onChange={(e) => updateLocalValue(key, e.target.value)}
            min="1"
            max="50"
            step="0.1"
            className="flex-1"
          />
          <Button
            size="sm"
            variant={isModified ? 'default' : 'secondary'}
            onClick={() => saveConfig(key)}
          >
            <Save className="h-4 w-4 mr-1" />
            Set
          </Button>
        </div>
      )
    }

    // Starting capital selector
    if (key === 'starting_capital') {
      const currentValue = parseFloat(value || '10000000').toFixed(0)
      return (
        <div className="flex gap-2">
          <Select value={currentValue} onValueChange={(v) => updateLocalValue(key, v)}>
            <SelectTrigger className="flex-1">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {CAPITAL_OPTIONS.map((option) => (
                <SelectItem key={option.value} value={option.value}>
                  {option.label}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          <Button
            size="sm"
            variant={isModified ? 'default' : 'secondary'}
            onClick={() => saveConfig(key)}
          >
            <Save className="h-4 w-4 mr-1" />
            Set
          </Button>
        </div>
      )
    }

    // Interval inputs
    if (key === 'order_check_interval' || key === 'mtm_update_interval') {
      return (
        <div className="flex gap-2">
          <Input
            type="number"
            value={value}
            onChange={(e) => updateLocalValue(key, e.target.value)}
            min={key === 'mtm_update_interval' ? 0 : 1}
            max={key === 'mtm_update_interval' ? 60 : 30}
            step="1"
            className="flex-1"
          />
          <Button
            size="sm"
            variant={isModified ? 'default' : 'secondary'}
            onClick={() => saveConfig(key)}
          >
            <Save className="h-4 w-4 mr-1" />
            Set
          </Button>
        </div>
      )
    }

    // Default text input
    return (
      <div className="flex gap-2">
        <Input
          type="text"
          value={value}
          onChange={(e) => updateLocalValue(key, e.target.value)}
          className="flex-1"
        />
        <Button
          size="sm"
          variant={isModified ? 'default' : 'secondary'}
          onClick={() => saveConfig(key)}
        >
          <Save className="h-4 w-4 mr-1" />
          Set
        </Button>
      </div>
    )
  }

  if (isLoading) {
    return (
      <div className="container mx-auto py-8 px-4 flex items-center justify-center">
        <div className="animate-spin rounded-full h-8 w-8 border-b-2 border-primary"></div>
      </div>
    )
  }

  return (
    <div className="container mx-auto py-6 px-4">
      {/* Header */}
      <div className="flex flex-col md:flex-row justify-between items-start md:items-center mb-8 gap-4">
        <div>
          <h1 className="text-3xl font-bold flex items-center gap-2">
            <Settings className="h-8 w-8" />
            Sandbox Configuration
          </h1>
          <p className="text-muted-foreground mt-1">Configure paper trading environment settings</p>
        </div>
        <div className="flex gap-3">
          <Button asChild>
            <Link to="/sandbox/mypnl">
              <BarChart3 className="h-4 w-4 mr-2" />
              My P&L
            </Link>
          </Button>
          <Button variant="destructive" onClick={() => setShowResetDialog(true)}>
            <RotateCcw className="h-4 w-4 mr-2" />
            Reset to Defaults
          </Button>
        </div>
      </div>

      {/* Configuration Sections */}
      <div className="space-y-6">
        {Object.entries(CONFIG_CATEGORIES).map(([categoryKey, category]) => (
          <Card key={categoryKey}>
            <CardHeader>
              <CardTitle className="flex items-center gap-2">
                <Settings className="h-5 w-5 text-primary" />
                {category.title}
              </CardTitle>
            </CardHeader>
            <CardContent>
              <div className="grid grid-cols-1 md:grid-cols-2 gap-6">
                {category.keys.map((key) => (
                  <div key={key} className="space-y-2">
                    <Label htmlFor={key} className="font-semibold">
                      {CONFIG_LABELS[key]}
                    </Label>
                    {renderConfigInput(key)}
                    <p className="text-xs text-muted-foreground">{CONFIG_DESCRIPTIONS[key]}</p>
                  </div>
                ))}
              </div>
            </CardContent>
          </Card>
        ))}
      </div>

      {/* Reset Confirmation Dialog */}
      <Dialog open={showResetDialog} onOpenChange={setShowResetDialog}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle className="flex items-center gap-2 text-destructive">
              <RotateCcw className="h-5 w-5" />
              Reset ALL Sandbox Data
            </DialogTitle>
            <DialogDescription asChild>
              <div className="space-y-4 pt-4">
                <Alert variant="destructive">
                  <AlertDescription>This action cannot be undone!</AlertDescription>
                </Alert>

                <div className="space-y-2">
                  <p className="font-semibold">This action will:</p>
                  <ul className="list-disc list-inside space-y-1 ml-4 text-sm">
                    <li>Delete all orders, trades, positions, and holdings</li>
                    <li>Reset funds to starting capital (1.00 Crore)</li>
                    <li>Clear all historical data</li>
                  </ul>
                </div>

                <p className="text-muted-foreground">
                  Are you absolutely sure you want to reset everything?
                </p>
              </div>
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="ghost" onClick={() => setShowResetDialog(false)}>
              Cancel
            </Button>
            <Button variant="destructive" onClick={resetConfiguration} disabled={isResetting}>
              {isResetting ? (
                <>
                  <div className="animate-spin rounded-full h-4 w-4 border-b-2 border-current mr-2"></div>
                  Resetting...
                </>
              ) : (
                'Yes, Reset Everything'
              )}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  )
}
