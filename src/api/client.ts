/**
 * API Client for OpenAlgo Desktop
 *
 * This module provides the API client for communicating with the backend.
 * In the desktop app, we use Tauri IPC instead of HTTP requests.
 */

// Re-export Tauri client as the primary API interface
export {
  authCommands,
  brokerCommands,
  fundsCommands,
  historifyCommands,
  holdingsCommands,
  orderCommands,
  positionCommands,
  quoteCommands,
  sandboxCommands,
  settingsCommands,
  strategyCommands,
  symbolCommands,
  tauri,
} from './tauri-client'

// ============================================================================
// Legacy HTTP stubs for compatibility
// These functions are not used in the desktop app but are imported by
// some pages that were designed for the web version.
// ============================================================================

/**
 * Stub web client for desktop compatibility.
 * In desktop mode, all API calls go through Tauri IPC, not HTTP.
 */
export const webClient = {
  get: async <T = any>(
    _url: string,
    _config?: Record<string, unknown>
  ): Promise<{ data: T | null }> => {
    console.warn('webClient.get is not available in desktop mode')
    return { data: null }
  },
  post: async <T = any>(
    _url: string,
    _data?: unknown,
    _config?: Record<string, unknown>
  ): Promise<{ data: T | null }> => {
    console.warn('webClient.post is not available in desktop mode')
    return { data: null }
  },
  put: async <T = any>(
    _url: string,
    _data?: unknown,
    _config?: Record<string, unknown>
  ): Promise<{ data: T | null }> => {
    console.warn('webClient.put is not available in desktop mode')
    return { data: null }
  },
  delete: async <T = any>(
    _url: string,
    _config?: Record<string, unknown>
  ): Promise<{ data: T | null }> => {
    console.warn('webClient.delete is not available in desktop mode')
    return { data: null }
  },
}

/**
 * Stub CSRF token fetcher for desktop compatibility.
 * Desktop app uses Tauri IPC which doesn't need CSRF tokens.
 */
export const fetchCSRFToken = async (): Promise<string> => {
  // Desktop app doesn't use CSRF tokens
  return ''
}

export type {
  BrokerCredentials,
  BrokerInfo,
  BrokerLoginRequest,
  BrokerLoginResponse,
  BrokerStatus,
  ClosePositionRequest,
  ClosePositionResponse,
  CreateStrategyRequest,
  DepthLevel,
  Funds,
  Holding,
  LoginRequest,
  LoginResponse,
  MarketDataQuery,
  MarketDataRow,
  MarketDepth,
  ModifyOrderRequest,
  Order,
  OrderRequest,
  OrderResponse,
  Position,
  Quote,
  QuoteRequest,
  SandboxOrder,
  SandboxOrderRequest,
  SandboxPosition,
  SaveBrokerCredentialsRequest,
  Settings,
  Strategy,
  SymbolSearchResult,
  TauriError,
  UpdateSettingsRequest,
  UpdateStrategyRequest,
  UserInfo,
} from './tauri-client'

// Default export
export { tauri as default } from './tauri-client'
