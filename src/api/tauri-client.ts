/**
 * Tauri IPC client for OpenAlgo Desktop
 *
 * This module provides type-safe wrappers around Tauri's invoke function
 * to communicate with the Rust backend.
 */

import { invoke } from '@tauri-apps/api/core'

// ============================================================================
// Types
// ============================================================================

export interface TauriError {
  code: string
  message: string
}

export interface LoginRequest {
  username: string
  password: string
}

export interface LoginResponse {
  success: boolean
  user_id: number
  username: string
}

export interface UserInfo {
  user_id: number
  username: string
  authenticated_at: string
}

export interface BrokerLoginRequest {
  broker_id: string
  credentials: BrokerCredentials
}

export interface BrokerCredentials {
  api_key: string
  api_secret?: string
  client_id?: string
  password?: string
  totp?: string
  request_token?: string
  auth_code?: string
}

export interface BrokerLoginResponse {
  success: boolean
  broker_id: string
  user_id: string
  user_name?: string
}

export interface BrokerStatus {
  connected: boolean
  broker_id?: string
  user_id?: string
  authenticated_at?: string
}

export interface BrokerInfo {
  id: string
  name: string
  logo: string
  requires_totp: boolean
}

export interface OrderRequest {
  symbol: string
  exchange: string
  side: string
  quantity: number
  price: number
  order_type: string
  product: string
  validity: string
  trigger_price?: number
  disclosed_quantity?: number
  amo: boolean
}

export interface ModifyOrderRequest {
  quantity?: number
  price?: number
  order_type?: string
  trigger_price?: number
  validity?: string
}

export interface OrderResponse {
  success: boolean
  order_id: string
  message?: string
}

export interface Order {
  order_id: string
  exchange_order_id?: string
  symbol: string
  exchange: string
  side: string
  quantity: number
  filled_quantity: number
  pending_quantity: number
  price: number
  trigger_price: number
  average_price: number
  order_type: string
  product: 'MIS' | 'NRML' | 'CNC'
  status: string
  validity: string
  order_timestamp: string
  exchange_timestamp?: string
  rejection_reason?: string
  // Additional fields for frontend compatibility
  action: 'BUY' | 'SELL'
  pricetype: 'MARKET' | 'LIMIT' | 'SL' | 'SL-M'
  orderid: string
  order_status: 'complete' | 'rejected' | 'cancelled' | 'open' | 'pending' | 'trigger pending'
  timestamp: string
}

export interface Position {
  symbol: string
  exchange: string
  product: 'MIS' | 'NRML' | 'CNC'
  quantity: number
  overnight_quantity: number
  average_price: number
  ltp: number
  pnl: number
  pnlpercent: number // Added for frontend compatibility
  realized_pnl: number
  unrealized_pnl: number
  buy_quantity: number
  buy_value: number
  sell_quantity: number
  sell_value: number
}

export interface Holding {
  symbol: string
  exchange: string
  isin?: string
  quantity: number
  t1_quantity: number
  average_price: number
  ltp: number
  close_price: number
  pnl: number
  pnl_percentage: number
  pnlpercent: number // Alias for frontend compatibility
  current_value: number
  product: string // Added for frontend compatibility
}

export interface Funds {
  available_cash: number
  used_margin: number
  total_margin: number
  opening_balance: number
  payin: number
  payout: number
  span: number
  exposure: number
  collateral: number
}

export interface QuoteRequest {
  exchange: string
  symbol: string
}

export interface Quote {
  symbol: string
  exchange: string
  ltp: number
  open: number
  high: number
  low: number
  close: number
  volume: number
  bid: number
  ask: number
  bid_qty: number
  ask_qty: number
  oi: number
  change: number
  change_percent: number
  timestamp: string
}

export interface MarketDepth {
  symbol: string
  exchange: string
  bids: DepthLevel[]
  asks: DepthLevel[]
}

export interface DepthLevel {
  price: number
  quantity: number
  orders: number
}

export interface SymbolSearchResult {
  symbol: string
  token: string
  exchange: string
  name: string
  instrument_type: string
  lot_size: number
  // Additional fields for frontend compatibility
  brsymbol: string
  lotsize: number
}

export interface Strategy {
  id: number
  name: string
  webhook_id: string
  exchange: string
  symbol: string
  product: string
  quantity: number
  enabled: boolean
  created_at: string
  updated_at: string
  // Additional fields for frontend compatibility
  platform: 'tradingview' | 'amibroker' | 'python' | 'metatrader' | 'excel' | 'others'
  is_active: boolean
  is_intraday: boolean
  trading_mode: 'LONG' | 'SHORT' | 'BOTH'
  start_time: string | null
  end_time: string | null
  squareoff_time: string | null
}

export interface CreateStrategyRequest {
  name: string
  webhook_id: string
  exchange: string
  symbol: string
  product: string
  quantity: number
  enabled: boolean
  // Additional fields for frontend compatibility
  platform?: string
  strategy_type?: string
  trading_mode?: string
  start_time?: string
  end_time?: string
  squareoff_time?: string
}

export interface UpdateStrategyRequest {
  id: number
  name?: string
  exchange?: string
  symbol?: string
  product?: string
  quantity?: number
  enabled?: boolean
}

export interface Settings {
  id: number
  theme: string
  default_broker?: string
  default_exchange: string
  default_product: string
  order_confirm: boolean
  sound_enabled: boolean
}

export interface UpdateSettingsRequest {
  theme?: string
  default_broker?: string
  default_exchange?: string
  default_product?: string
  order_confirm?: boolean
  sound_enabled?: boolean
}

export interface SaveBrokerCredentialsRequest {
  broker_id: string
  api_key: string
  api_secret?: string
  client_id?: string
}

export interface SandboxOrder {
  id: number
  order_id: string
  symbol: string
  exchange: string
  side: string
  quantity: number
  price: number
  order_type: string
  product: string
  status: string
  filled_quantity: number
  average_price: number
  created_at: string
  updated_at: string
}

export interface SandboxPosition {
  id: number
  symbol: string
  exchange: string
  product: string
  quantity: number
  average_price: number
  ltp: number
  pnl: number
  created_at: string
  updated_at: string
}

export interface SandboxOrderRequest {
  symbol: string
  exchange: string
  side: string
  quantity: number
  price: number
  order_type: string
  product: string
}

export interface MarketDataQuery {
  symbol: string
  exchange: string
  timeframe: string
  from_date: string
  to_date: string
}

export interface MarketDataRow {
  timestamp: string
  open: number
  high: number
  low: number
  close: number
  volume: number
}

export interface ClosePositionRequest {
  symbol: string
  exchange: string
  product: string
  quantity: number
  position_type: string
}

export interface ClosePositionResponse {
  success: boolean
  order_id?: string
  message: string
}

// ============================================================================
// Tauri Command Wrappers
// ============================================================================

/**
 * Generic invoke wrapper with error handling
 */
async function tauriInvoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(command, args)
  } catch (error) {
    // Tauri errors come as objects with code and message
    const tauriError = error as TauriError
    throw new Error(tauriError.message || String(error))
  }
}

// ============================================================================
// Auth Commands
// ============================================================================

export const authCommands = {
  login: (request: LoginRequest) => tauriInvoke<LoginResponse>('login', { request }),

  logout: () => tauriInvoke<void>('logout'),

  checkSession: () => tauriInvoke<boolean>('check_session'),

  getCurrentUser: () => tauriInvoke<UserInfo | null>('get_current_user'),
}

// ============================================================================
// Broker Commands
// ============================================================================

export const brokerCommands = {
  brokerLogin: (request: BrokerLoginRequest) =>
    tauriInvoke<BrokerLoginResponse>('broker_login', { request }),

  brokerLogout: () => tauriInvoke<void>('broker_logout'),

  getBrokerStatus: () => tauriInvoke<BrokerStatus>('get_broker_status'),

  setActiveBroker: (brokerId: string) =>
    tauriInvoke<void>('set_active_broker', { broker_id: brokerId }),

  getAvailableBrokers: () => tauriInvoke<BrokerInfo[]>('get_available_brokers'),
}

// ============================================================================
// Order Commands
// ============================================================================

export const orderCommands = {
  placeOrder: (order: OrderRequest) => tauriInvoke<OrderResponse>('place_order', { order }),

  modifyOrder: (orderId: string, order: ModifyOrderRequest) =>
    tauriInvoke<OrderResponse>('modify_order', { order_id: orderId, order }),

  cancelOrder: (orderId: string, variety?: string) =>
    tauriInvoke<OrderResponse>('cancel_order', { order_id: orderId, variety }),

  getOrderBook: () => tauriInvoke<Order[]>('get_order_book'),

  getTradeBook: () => tauriInvoke<Order[]>('get_trade_book'),
}

// ============================================================================
// Position Commands
// ============================================================================

export const positionCommands = {
  getPositions: () => tauriInvoke<Position[]>('get_positions'),

  closePosition: (request: ClosePositionRequest) =>
    tauriInvoke<ClosePositionResponse>('close_position', { request }),

  closeAllPositions: () => tauriInvoke<ClosePositionResponse[]>('close_all_positions'),
}

// ============================================================================
// Holdings Commands
// ============================================================================

export const holdingsCommands = {
  getHoldings: () => tauriInvoke<Holding[]>('get_holdings'),
}

// ============================================================================
// Funds Commands
// ============================================================================

export const fundsCommands = {
  getFunds: () => tauriInvoke<Funds>('get_funds'),
}

// ============================================================================
// Quote Commands
// ============================================================================

export const quoteCommands = {
  getQuote: (symbols: QuoteRequest[]) => tauriInvoke<Quote[]>('get_quote', { symbols }),

  getMarketDepth: (exchange: string, symbol: string) =>
    tauriInvoke<MarketDepth>('get_market_depth', { exchange, symbol }),
}

// ============================================================================
// Symbol Commands
// ============================================================================

export const symbolCommands = {
  searchSymbols: (query: string, exchange?: string, limit?: number) =>
    tauriInvoke<SymbolSearchResult[]>('search_symbols', { query, exchange, limit }),

  getSymbolInfo: (exchange: string, symbol: string) =>
    tauriInvoke<SymbolSearchResult>('get_symbol_info', { exchange, symbol }),

  refreshSymbolMaster: () => tauriInvoke<number>('refresh_symbol_master'),
}

// ============================================================================
// Strategy Commands
// ============================================================================

export const strategyCommands = {
  getStrategies: () => tauriInvoke<Strategy[]>('get_strategies'),

  createStrategy: (request: CreateStrategyRequest) =>
    tauriInvoke<Strategy>('create_strategy', { request }),

  updateStrategy: (request: UpdateStrategyRequest) =>
    tauriInvoke<Strategy>('update_strategy', { request }),

  deleteStrategy: (id: number) => tauriInvoke<void>('delete_strategy', { id }),

  toggleStrategy: (id: number, enabled: boolean) =>
    tauriInvoke<Strategy>('toggle_strategy', { id, enabled }),
}

// ============================================================================
// Settings Commands
// ============================================================================

export const settingsCommands = {
  getSettings: () => tauriInvoke<Settings>('get_settings'),

  updateSettings: (request: UpdateSettingsRequest) =>
    tauriInvoke<Settings>('update_settings', { request }),

  saveBrokerCredentials: (request: SaveBrokerCredentialsRequest) =>
    tauriInvoke<void>('save_broker_credentials', { request }),

  deleteBrokerCredentials: (brokerId: string) =>
    tauriInvoke<void>('delete_broker_credentials', { broker_id: brokerId }),
}

// ============================================================================
// Sandbox Commands
// ============================================================================

export const sandboxCommands = {
  getSandboxPositions: () => tauriInvoke<SandboxPosition[]>('get_sandbox_positions'),

  getSandboxOrders: () => tauriInvoke<SandboxOrder[]>('get_sandbox_orders'),

  placeSandboxOrder: (order: SandboxOrderRequest) =>
    tauriInvoke<SandboxOrder>('place_sandbox_order', { order }),

  resetSandbox: () => tauriInvoke<void>('reset_sandbox'),
}

// ============================================================================
// Historify Commands
// ============================================================================

export const historifyCommands = {
  getMarketData: (query: MarketDataQuery) =>
    tauriInvoke<MarketDataRow[]>('get_market_data', { query }),

  downloadHistoricalData: (request: MarketDataQuery) =>
    tauriInvoke<{ success: boolean; rows_downloaded: number; message: string }>(
      'download_historical_data',
      { request }
    ),
}

// ============================================================================
// Convenience exports
// ============================================================================

export const tauri = {
  auth: authCommands,
  broker: brokerCommands,
  orders: orderCommands,
  positions: positionCommands,
  holdings: holdingsCommands,
  funds: fundsCommands,
  quotes: quoteCommands,
  symbols: symbolCommands,
  strategies: strategyCommands,
  settings: settingsCommands,
  sandbox: sandboxCommands,
  historify: historifyCommands,
}

export default tauri
