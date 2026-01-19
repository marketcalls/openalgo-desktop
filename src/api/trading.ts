/**
 * Trading API for OpenAlgo Desktop
 *
 * Uses Tauri IPC commands for trading operations.
 */

import type {
  Funds,
  Holding,
  MarketDepth,
  ModifyOrderRequest,
  Order,
  OrderRequest,
  OrderResponse,
  Position,
  Quote,
} from './client'
import {
  fundsCommands,
  holdingsCommands,
  orderCommands,
  positionCommands,
  quoteCommands,
} from './client'

// Re-export types
export type {
  Order,
  OrderRequest,
  OrderResponse,
  ModifyOrderRequest,
  Position,
  Holding,
  Funds,
  Quote,
  MarketDepth,
}

// Legacy types for compatibility with existing pages
export interface QuotesData {
  ask: number
  bid: number
  high: number
  low: number
  ltp: number
  oi: number
  open: number
  prev_close: number
  volume: number
}

export interface MultiQuotesSymbol {
  symbol: string
  exchange: string
}

export interface MultiQuotesResult {
  symbol: string
  exchange: string
  data: QuotesData
}

export interface ApiResponse<T> {
  status: 'success' | 'error' | 'info'
  data?: T
  message?: string
}

export interface MarginData {
  availablecash: number
  collateral: number
  m2mrealized: number
  m2munrealized: number
  utiliseddebits: number
}

export interface Trade {
  symbol: string
  exchange: string
  action: 'BUY' | 'SELL'
  quantity: number
  average_price: number
  trade_value: number
  product: string
  orderid: string
  timestamp: string
  trade_id?: string
}

export interface OrderStats {
  total_buy_orders: number
  total_sell_orders: number
  total_completed_orders: number
  total_open_orders: number
  total_rejected_orders: number
}

export interface PortfolioStats {
  totalholdingvalue: number
  totalinvvalue: number
  totalprofitandloss: number
  totalpnlpercentage: number
}

export interface PlaceOrderRequest {
  apikey?: string // Not needed for desktop but kept for compatibility
  symbol: string
  exchange: string
  action: string
  product: string
  pricetype: string
  price: number
  quantity: number
  trigger_price?: number
  disclosed_quantity?: number
}

// Convert Quote to QuotesData format
function toQuotesData(quote: Quote): QuotesData {
  return {
    ask: quote.ask,
    bid: quote.bid,
    high: quote.high,
    low: quote.low,
    ltp: quote.ltp,
    oi: quote.oi,
    open: quote.open,
    prev_close: quote.close,
    volume: quote.volume,
  }
}

// Convert Position to legacy format if needed
function toMarginData(funds: Funds): MarginData {
  return {
    availablecash: funds.available_cash,
    collateral: funds.collateral,
    m2mrealized: 0,
    m2munrealized: 0,
    utiliseddebits: funds.used_margin,
  }
}

export const tradingApi = {
  /**
   * Get real-time quotes for a symbol
   */
  getQuotes: async (
    _apiKey: string, // Ignored in desktop - auth is session-based
    symbol: string,
    exchange: string
  ): Promise<ApiResponse<QuotesData>> => {
    try {
      const quotes = await quoteCommands.getQuote([{ exchange, symbol }])
      if (quotes.length > 0) {
        return {
          status: 'success',
          data: toQuotesData(quotes[0]),
        }
      }
      return {
        status: 'error',
        message: 'No quote data returned',
      }
    } catch (error) {
      return {
        status: 'error',
        message: error instanceof Error ? error.message : 'Unknown error',
      }
    }
  },

  /**
   * Get real-time quotes for multiple symbols
   */
  getMultiQuotes: async (
    _apiKey: string,
    symbols: MultiQuotesSymbol[]
  ): Promise<{ status: string; results?: MultiQuotesResult[]; message?: string }> => {
    try {
      const quotes = await quoteCommands.getQuote(
        symbols.map((s) => ({ exchange: s.exchange, symbol: s.symbol }))
      )
      return {
        status: 'success',
        results: quotes.map((q) => ({
          symbol: q.symbol,
          exchange: q.exchange,
          data: toQuotesData(q),
        })),
      }
    } catch (error) {
      return {
        status: 'error',
        message: error instanceof Error ? error.message : 'Unknown error',
      }
    }
  },

  /**
   * Get margin/funds data
   */
  getFunds: async (_apiKey: string): Promise<ApiResponse<MarginData>> => {
    try {
      const funds = await fundsCommands.getFunds()
      return {
        status: 'success',
        data: toMarginData(funds),
      }
    } catch (error) {
      return {
        status: 'error',
        message: error instanceof Error ? error.message : 'Unknown error',
      }
    }
  },

  /**
   * Get positions
   */
  getPositions: async (_apiKey: string): Promise<ApiResponse<Position[]>> => {
    try {
      const positions = await positionCommands.getPositions()
      return {
        status: 'success',
        data: positions,
      }
    } catch (error) {
      return {
        status: 'error',
        message: error instanceof Error ? error.message : 'Unknown error',
      }
    }
  },

  /**
   * Get order book
   */
  getOrders: async (
    _apiKey: string
  ): Promise<ApiResponse<{ orders: Order[]; statistics: OrderStats }>> => {
    try {
      const orders = await orderCommands.getOrderBook()

      // Calculate statistics
      const stats: OrderStats = {
        total_buy_orders: orders.filter((o) => o.side === 'BUY' || o.action === 'BUY').length,
        total_sell_orders: orders.filter((o) => o.side === 'SELL' || o.action === 'SELL').length,
        total_completed_orders: orders.filter((o) => o.status === 'complete').length,
        total_open_orders: orders.filter((o) => o.status === 'pending' || o.status === 'open')
          .length,
        total_rejected_orders: orders.filter((o) => o.status === 'rejected').length,
      }

      return {
        status: 'success',
        data: { orders, statistics: stats },
      }
    } catch (error) {
      return {
        status: 'error',
        message: error instanceof Error ? error.message : 'Unknown error',
      }
    }
  },

  /**
   * Get trade book
   */
  getTrades: async (_apiKey: string): Promise<ApiResponse<Trade[]>> => {
    try {
      const trades = await orderCommands.getTradeBook()
      return {
        status: 'success',
        data: trades.map(
          (t): Trade => ({
            symbol: t.symbol,
            exchange: t.exchange,
            action: t.action || (t.side === 'BUY' ? 'BUY' : 'SELL'),
            quantity: t.quantity,
            average_price: t.average_price,
            trade_value: t.average_price * t.quantity,
            product: t.product,
            orderid: t.orderid || t.order_id,
            timestamp: t.timestamp || t.order_timestamp,
            trade_id: t.exchange_order_id || t.order_id,
          })
        ),
      }
    } catch (error) {
      return {
        status: 'error',
        message: error instanceof Error ? error.message : 'Unknown error',
      }
    }
  },

  /**
   * Get holdings
   */
  getHoldings: async (
    _apiKey: string
  ): Promise<ApiResponse<{ holdings: Holding[]; statistics: PortfolioStats }>> => {
    try {
      const holdings = await holdingsCommands.getHoldings()

      // Calculate statistics
      const totalInvestment = holdings.reduce((sum, h) => sum + h.average_price * h.quantity, 0)
      const currentValue = holdings.reduce((sum, h) => sum + h.current_value, 0)
      const totalPnl = holdings.reduce((sum, h) => sum + h.pnl, 0)
      const totalPnlPercent = totalInvestment > 0 ? (totalPnl / totalInvestment) * 100 : 0

      const stats: PortfolioStats = {
        totalholdingvalue: currentValue,
        totalinvvalue: totalInvestment,
        totalprofitandloss: totalPnl,
        totalpnlpercentage: totalPnlPercent,
      }

      return {
        status: 'success',
        data: { holdings, statistics: stats },
      }
    } catch (error) {
      return {
        status: 'error',
        message: error instanceof Error ? error.message : 'Unknown error',
      }
    }
  },

  /**
   * Place order
   */
  placeOrder: async (order: PlaceOrderRequest): Promise<ApiResponse<{ orderid: string }>> => {
    try {
      const response = await orderCommands.placeOrder({
        symbol: order.symbol,
        exchange: order.exchange,
        side: order.action,
        quantity: order.quantity,
        price: order.price,
        order_type: order.pricetype,
        product: order.product,
        validity: 'DAY',
        trigger_price: order.trigger_price,
        disclosed_quantity: order.disclosed_quantity,
        amo: false,
      })

      return {
        status: 'success',
        data: { orderid: response.order_id },
      }
    } catch (error) {
      return {
        status: 'error',
        message: error instanceof Error ? error.message : 'Unknown error',
      }
    }
  },

  /**
   * Modify order
   */
  modifyOrder: async (
    orderid: string,
    orderData: {
      symbol: string
      exchange: string
      action: string
      product: string
      pricetype: string
      price: number
      quantity: number
      trigger_price?: number
      disclosed_quantity?: number
    }
  ): Promise<ApiResponse<{ orderid: string }>> => {
    try {
      const response = await orderCommands.modifyOrder(orderid, {
        quantity: orderData.quantity,
        price: orderData.price,
        order_type: orderData.pricetype,
        trigger_price: orderData.trigger_price,
      })

      return {
        status: 'success',
        data: { orderid: response.order_id },
      }
    } catch (error) {
      return {
        status: 'error',
        message: error instanceof Error ? error.message : 'Unknown error',
      }
    }
  },

  /**
   * Cancel order
   */
  cancelOrder: async (orderid: string): Promise<ApiResponse<{ orderid: string }>> => {
    try {
      const response = await orderCommands.cancelOrder(orderid)
      return {
        status: 'success',
        data: { orderid: response.order_id },
      }
    } catch (error) {
      return {
        status: 'error',
        message: error instanceof Error ? error.message : 'Unknown error',
      }
    }
  },

  /**
   * Close a specific position
   */
  closePosition: async (
    symbol: string,
    exchange: string,
    product: string
  ): Promise<ApiResponse<void>> => {
    try {
      // Get current position to determine quantity and direction
      const positions = await positionCommands.getPositions()
      const position = positions.find(
        (p) => p.symbol === symbol && p.exchange === exchange && p.product === product
      )

      if (!position || position.quantity === 0) {
        return {
          status: 'error',
          message: 'Position not found or already closed',
        }
      }

      await positionCommands.closePosition({
        symbol,
        exchange,
        product,
        quantity: Math.abs(position.quantity),
        position_type: position.quantity > 0 ? 'long' : 'short',
      })

      return { status: 'success' }
    } catch (error) {
      return {
        status: 'error',
        message: error instanceof Error ? error.message : 'Unknown error',
      }
    }
  },

  /**
   * Close all positions
   */
  closeAllPositions: async (): Promise<ApiResponse<void>> => {
    try {
      await positionCommands.closeAllPositions()
      return { status: 'success' }
    } catch (error) {
      return {
        status: 'error',
        message: error instanceof Error ? error.message : 'Unknown error',
      }
    }
  },

  /**
   * Cancel all orders
   */
  cancelAllOrders: async (): Promise<ApiResponse<void>> => {
    try {
      const orders = await orderCommands.getOrderBook()
      const pendingOrders = orders.filter(
        (o) => o.status === 'pending' || o.status === 'open' || o.status === 'trigger_pending'
      )

      await Promise.all(pendingOrders.map((o) => orderCommands.cancelOrder(o.order_id)))

      return { status: 'success' }
    } catch (error) {
      return {
        status: 'error',
        message: error instanceof Error ? error.message : 'Unknown error',
      }
    }
  },

  /**
   * Get market depth
   */
  getMarketDepth: async (exchange: string, symbol: string): Promise<MarketDepth> => {
    return quoteCommands.getMarketDepth(exchange, symbol)
  },
}

export default tradingApi
