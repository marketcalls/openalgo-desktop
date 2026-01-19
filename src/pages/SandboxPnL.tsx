import { Activity, Briefcase, Calendar, Package, Radio, RefreshCw, Settings } from 'lucide-react'
import { useCallback, useEffect, useMemo, useState } from 'react'
import { Link } from 'react-router-dom'
import {
  sandboxCommands,
  type SandboxDailyPnl,
  type SandboxHolding,
  type SandboxPosition,
  type SandboxPnlData,
  type SandboxPnlSummary,
  type SandboxTrade,
} from '@/api/tauri-client'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { useLivePrice } from '@/hooks/useLivePrice'
import { cn } from '@/lib/utils'

function formatCurrency(value: number): string {
  return new Intl.NumberFormat('en-IN', {
    minimumFractionDigits: 2,
    maximumFractionDigits: 2,
  }).format(value)
}

function getPnLColor(value: number): string {
  if (value > 0) return 'text-green-500'
  if (value < 0) return 'text-red-500'
  return ''
}

export default function SandboxPnL() {
  const [data, setData] = useState<SandboxPnlData | null>(null)
  const [isLoading, setIsLoading] = useState(true)
  const [isRefreshing, setIsRefreshing] = useState(false)
  const [activeTab, setActiveTab] = useState('daily')

  const fetchData = useCallback(async (showRefresh = false) => {
    if (showRefresh) setIsRefreshing(true)
    try {
      const result = await sandboxCommands.getSandboxPnl()
      setData(result)
    } catch (error) {
      console.error('Error fetching sandbox P&L:', error)
    } finally {
      setIsLoading(false)
      setIsRefreshing(false)
    }
  }, [])

  useEffect(() => {
    fetchData()
    // Poll every 30 seconds
    const interval = setInterval(() => fetchData(), 30000)
    return () => clearInterval(interval)
  }, [fetchData])

  // Convert positions for useLivePrice hook
  const positionsForLivePrice = useMemo(() => {
    if (!data?.positions) return []
    return data.positions.map((p) => ({
      symbol: p.symbol,
      exchange: p.exchange,
      ltp: p.ltp,
      pnl: p.pnl,
      quantity: p.quantity,
      average_price: p.average_price,
    }))
  }, [data?.positions])

  // Get live prices for positions
  const { data: livePositions, isLive } = useLivePrice(positionsForLivePrice, {
    enabled: positionsForLivePrice.length > 0,
    useMultiQuotesFallback: true,
  })

  // Merge live prices back into positions
  const enhancedPositions = useMemo((): SandboxPosition[] => {
    if (!data?.positions) return []
    if (livePositions.length === 0) return data.positions

    return data.positions.map((pos) => {
      const livePos = livePositions.find(
        (lp) => lp.symbol === pos.symbol && lp.exchange === pos.exchange
      )
      if (livePos && livePos.ltp !== undefined) {
        const newPnl = pos.quantity * (livePos.ltp - pos.average_price)
        return {
          ...pos,
          ltp: livePos.ltp,
          pnl: newPnl,
        }
      }
      return pos
    })
  }, [data?.positions, livePositions])

  // Calculate enhanced summary with live prices
  const enhancedSummary = useMemo((): SandboxPnlSummary | null => {
    if (!data?.summary) return null
    if (!isLive || enhancedPositions.length === 0) return data.summary

    const positionsUnrealizedPnl = enhancedPositions.reduce((sum, p) => sum + p.pnl, 0)
    return {
      ...data.summary,
      positions_unrealized_pnl: positionsUnrealizedPnl,
      today_total_mtm: data.summary.today_realized_pnl + positionsUnrealizedPnl,
    }
  }, [data?.summary, enhancedPositions, isLive])

  if (isLoading) {
    return (
      <div className="container mx-auto py-8 px-4 flex items-center justify-center">
        <div className="animate-spin rounded-full h-8 w-8 border-b-2 border-primary"></div>
      </div>
    )
  }

  const summary = enhancedSummary || data?.summary
  const dailyPnl = data?.daily_pnl || []
  const positions = enhancedPositions.length > 0 ? enhancedPositions : (data?.positions || [])
  const holdings = data?.holdings || []
  const trades = data?.trades || []

  return (
    <div className="container mx-auto py-6 px-4">
      {/* Header */}
      <div className="flex flex-col md:flex-row justify-between items-start md:items-center mb-8 gap-4">
        <div>
          <div className="flex items-center gap-3">
            <h1 className="text-3xl font-bold flex items-center gap-2">
              <Activity className="h-8 w-8" />
              Sandbox P&L
            </h1>
            {isLive && (
              <Badge
                variant="outline"
                className="bg-emerald-500/10 text-emerald-600 border-emerald-500/30 gap-1"
              >
                <Radio className="h-3 w-3 animate-pulse" />
                Live
              </Badge>
            )}
          </div>
          <p className="text-muted-foreground mt-1">Paper trading performance overview</p>
        </div>
        <div className="flex gap-3">
          <Button variant="outline" size="sm" onClick={() => fetchData(true)} disabled={isRefreshing}>
            <RefreshCw className={cn('h-4 w-4 mr-2', isRefreshing && 'animate-spin')} />
            Refresh
          </Button>
          <Button asChild>
            <Link to="/sandbox">
              <Settings className="h-4 w-4 mr-2" />
              Configuration
            </Link>
          </Button>
        </div>
      </div>

      {/* Summary Cards */}
      <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-4 gap-4 mb-6">
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm font-medium text-muted-foreground">
              Today's Realized P&L
            </CardTitle>
          </CardHeader>
          <CardContent>
            <div className={cn('text-2xl font-bold', getPnLColor(summary?.today_realized_pnl || 0))}>
              {summary ? formatCurrency(summary.today_realized_pnl) : '---'}
            </div>
          </CardContent>
        </Card>
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm font-medium text-muted-foreground">
              Positions Unrealized
            </CardTitle>
          </CardHeader>
          <CardContent>
            <div
              className={cn('text-2xl font-bold', getPnLColor(summary?.positions_unrealized_pnl || 0))}
            >
              {summary ? formatCurrency(summary.positions_unrealized_pnl) : '---'}
            </div>
          </CardContent>
        </Card>
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm font-medium text-muted-foreground">Today's MTM</CardTitle>
          </CardHeader>
          <CardContent>
            <div className={cn('text-2xl font-bold', getPnLColor(summary?.today_total_mtm || 0))}>
              {summary ? formatCurrency(summary.today_total_mtm) : '---'}
            </div>
          </CardContent>
        </Card>
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm font-medium text-muted-foreground">Portfolio Value</CardTitle>
          </CardHeader>
          <CardContent>
            <div className="text-2xl font-bold text-primary">
              {summary ? formatCurrency(summary.portfolio_value) : '---'}
            </div>
          </CardContent>
        </Card>
      </div>

      {/* Tabs */}
      <Tabs value={activeTab} onValueChange={setActiveTab}>
        <TabsList className="mb-4">
          <TabsTrigger value="daily" className="gap-2">
            <Calendar className="h-4 w-4" />
            Daily P&L
          </TabsTrigger>
          <TabsTrigger value="positions" className="gap-2">
            <Briefcase className="h-4 w-4" />
            Positions ({positions.length})
          </TabsTrigger>
          <TabsTrigger value="holdings" className="gap-2">
            <Package className="h-4 w-4" />
            Holdings ({holdings.length})
          </TabsTrigger>
          <TabsTrigger value="trades" className="gap-2">
            <Activity className="h-4 w-4" />
            Trades ({trades.length})
          </TabsTrigger>
        </TabsList>

        {/* Daily P&L Tab */}
        <TabsContent value="daily">
          <Card>
            <CardContent className="p-0">
              {dailyPnl.length === 0 ? (
                <div className="py-12 text-center text-muted-foreground">No daily P&L data</div>
              ) : (
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>Date</TableHead>
                      <TableHead className="text-right">Realized P&L</TableHead>
                      <TableHead className="text-right">Unrealized P&L</TableHead>
                      <TableHead className="text-right">Total P&L</TableHead>
                      <TableHead className="text-right">Portfolio Value</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {dailyPnl.map((day) => (
                      <TableRow key={day.id}>
                        <TableCell>{day.date}</TableCell>
                        <TableCell className={cn('text-right', getPnLColor(day.realized_pnl))}>
                          {formatCurrency(day.realized_pnl)}
                        </TableCell>
                        <TableCell className={cn('text-right', getPnLColor(day.unrealized_pnl))}>
                          {formatCurrency(day.unrealized_pnl)}
                        </TableCell>
                        <TableCell className={cn('text-right font-medium', getPnLColor(day.total_pnl))}>
                          {formatCurrency(day.total_pnl)}
                        </TableCell>
                        <TableCell className="text-right">{formatCurrency(day.portfolio_value)}</TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              )}
            </CardContent>
          </Card>
        </TabsContent>

        {/* Positions Tab */}
        <TabsContent value="positions">
          <Card>
            <CardContent className="p-0">
              {positions.length === 0 ? (
                <div className="py-12 text-center text-muted-foreground">No open positions</div>
              ) : (
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>Symbol</TableHead>
                      <TableHead>Exchange</TableHead>
                      <TableHead>Product</TableHead>
                      <TableHead className="text-right">Quantity</TableHead>
                      <TableHead className="text-right">Avg Price</TableHead>
                      <TableHead className="text-right">LTP</TableHead>
                      <TableHead className="text-right">P&L</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {positions.map((pos) => (
                      <TableRow key={pos.id}>
                        <TableCell className="font-medium">{pos.symbol}</TableCell>
                        <TableCell>
                          <Badge variant="outline">{pos.exchange}</Badge>
                        </TableCell>
                        <TableCell>
                          <Badge variant="secondary">{pos.product}</Badge>
                        </TableCell>
                        <TableCell
                          className={cn('text-right', pos.quantity > 0 ? 'text-green-500' : 'text-red-500')}
                        >
                          {pos.quantity}
                        </TableCell>
                        <TableCell className="text-right">{formatCurrency(pos.average_price)}</TableCell>
                        <TableCell className="text-right font-mono">{formatCurrency(pos.ltp)}</TableCell>
                        <TableCell className={cn('text-right font-medium', getPnLColor(pos.pnl))}>
                          {formatCurrency(pos.pnl)}
                        </TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              )}
            </CardContent>
          </Card>
        </TabsContent>

        {/* Holdings Tab */}
        <TabsContent value="holdings">
          <Card>
            <CardContent className="p-0">
              {holdings.length === 0 ? (
                <div className="py-12 text-center text-muted-foreground">No holdings</div>
              ) : (
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>Symbol</TableHead>
                      <TableHead>Exchange</TableHead>
                      <TableHead className="text-right">Quantity</TableHead>
                      <TableHead className="text-right">Avg Price</TableHead>
                      <TableHead className="text-right">LTP</TableHead>
                      <TableHead className="text-right">P&L</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {holdings.map((h) => (
                      <TableRow key={h.id}>
                        <TableCell className="font-medium">{h.symbol}</TableCell>
                        <TableCell>
                          <Badge variant="outline">{h.exchange}</Badge>
                        </TableCell>
                        <TableCell className="text-right">{h.quantity}</TableCell>
                        <TableCell className="text-right">{formatCurrency(h.average_price)}</TableCell>
                        <TableCell className="text-right font-mono">{formatCurrency(h.ltp)}</TableCell>
                        <TableCell className={cn('text-right font-medium', getPnLColor(h.pnl))}>
                          {formatCurrency(h.pnl)}
                        </TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              )}
            </CardContent>
          </Card>
        </TabsContent>

        {/* Trades Tab */}
        <TabsContent value="trades">
          <Card>
            <CardContent className="p-0">
              {trades.length === 0 ? (
                <div className="py-12 text-center text-muted-foreground">No trades</div>
              ) : (
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>Trade ID</TableHead>
                      <TableHead>Symbol</TableHead>
                      <TableHead>Exchange</TableHead>
                      <TableHead>Side</TableHead>
                      <TableHead className="text-right">Quantity</TableHead>
                      <TableHead className="text-right">Price</TableHead>
                      <TableHead>Time</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {trades.map((trade) => (
                      <TableRow key={trade.id}>
                        <TableCell className="font-mono text-xs">{trade.trade_id}</TableCell>
                        <TableCell className="font-medium">{trade.symbol}</TableCell>
                        <TableCell>
                          <Badge variant="outline">{trade.exchange}</Badge>
                        </TableCell>
                        <TableCell>
                          <Badge
                            variant={trade.side === 'BUY' ? 'default' : 'destructive'}
                            className={trade.side === 'BUY' ? 'bg-green-500' : ''}
                          >
                            {trade.side}
                          </Badge>
                        </TableCell>
                        <TableCell className="text-right">{trade.quantity}</TableCell>
                        <TableCell className="text-right">{formatCurrency(trade.price)}</TableCell>
                        <TableCell className="text-xs text-muted-foreground">{trade.created_at}</TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              )}
            </CardContent>
          </Card>
        </TabsContent>
      </Tabs>
    </div>
  )
}
