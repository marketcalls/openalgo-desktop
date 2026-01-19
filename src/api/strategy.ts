/**
 * Strategy API for OpenAlgo Desktop
 *
 * Uses Tauri IPC commands for strategy operations.
 */

import type {
  CreateStrategyRequest,
  Strategy,
  SymbolSearchResult,
  UpdateStrategyRequest,
} from './client'
import { strategyCommands, symbolCommands } from './client'

// Re-export types
export type { Strategy, SymbolSearchResult }

// Legacy types for compatibility
export interface StrategySymbolMapping {
  id: number
  strategy_id: number
  exchange: string
  symbol: string
  quantity: number
  product_type: 'MIS' | 'CNC' | 'NRML'
  created_at: string
}

export interface AddSymbolRequest {
  exchange: string
  symbol: string
  quantity: number
  product_type: string
}

export interface ApiResponse<T> {
  status: 'success' | 'error'
  data?: T
  message?: string
}

export const strategyApi = {
  /**
   * Get all strategies
   */
  getStrategies: async (): Promise<Strategy[]> => {
    return strategyCommands.getStrategies()
  },

  /**
   * Get a single strategy by ID
   */
  getStrategy: async (
    strategyId: number
  ): Promise<{ strategy: Strategy; mappings: StrategySymbolMapping[] }> => {
    const strategies = await strategyCommands.getStrategies()
    const strategy = strategies.find((s) => s.id === strategyId)

    if (!strategy) {
      throw new Error(`Strategy not found: ${strategyId}`)
    }

    // For desktop, mappings are stored differently - single symbol per strategy
    const mappings: StrategySymbolMapping[] = [
      {
        id: strategy.id,
        strategy_id: strategy.id,
        exchange: strategy.exchange,
        symbol: strategy.symbol,
        quantity: strategy.quantity,
        product_type: (strategy.product as 'MIS' | 'CNC' | 'NRML') || 'MIS',
        created_at: strategy.created_at,
      },
    ]

    return { strategy, mappings }
  },

  /**
   * Create a new strategy
   * Accepts partial data from the frontend and fills in defaults for Tauri
   */
  createStrategy: async (data: {
    name: string
    platform?: string
    strategy_type?: string
    trading_mode?: string
    start_time?: string
    end_time?: string
    squareoff_time?: string
    webhook_id?: string
    exchange?: string
    symbol?: string
    product?: string
    quantity?: number
    enabled?: boolean
  }): Promise<ApiResponse<{ strategy_id: number }>> => {
    try {
      // Generate webhook ID if not provided
      const webhookId = data.webhook_id || crypto.randomUUID()

      const strategy = await strategyCommands.createStrategy({
        name: data.name,
        webhook_id: webhookId,
        exchange: data.exchange || '',
        symbol: data.symbol || '',
        product: data.product || 'MIS',
        quantity: data.quantity || 1,
        enabled: data.enabled ?? true,
        platform: data.platform,
        strategy_type: data.strategy_type,
        trading_mode: data.trading_mode,
        start_time: data.start_time,
        end_time: data.end_time,
        squareoff_time: data.squareoff_time,
      })

      return {
        status: 'success',
        data: { strategy_id: strategy.id },
      }
    } catch (error) {
      return {
        status: 'error',
        message: error instanceof Error ? error.message : 'Unknown error',
      }
    }
  },

  /**
   * Update a strategy
   */
  updateStrategy: async (
    strategyId: number,
    data: Partial<Omit<UpdateStrategyRequest, 'id'>>
  ): Promise<ApiResponse<void>> => {
    try {
      await strategyCommands.updateStrategy({
        id: strategyId,
        ...data,
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
   * Toggle strategy active/inactive
   */
  toggleStrategy: async (strategyId: number): Promise<ApiResponse<{ is_active: boolean }>> => {
    try {
      // Get current state
      const strategies = await strategyCommands.getStrategies()
      const strategy = strategies.find((s) => s.id === strategyId)

      if (!strategy) {
        return {
          status: 'error',
          message: 'Strategy not found',
        }
      }

      // Toggle
      const updated = await strategyCommands.toggleStrategy(strategyId, !strategy.enabled)

      return {
        status: 'success',
        data: { is_active: updated.enabled },
      }
    } catch (error) {
      return {
        status: 'error',
        message: error instanceof Error ? error.message : 'Unknown error',
      }
    }
  },

  /**
   * Delete a strategy
   */
  deleteStrategy: async (strategyId: number): Promise<ApiResponse<void>> => {
    try {
      await strategyCommands.deleteStrategy(strategyId)
      return { status: 'success' }
    } catch (error) {
      return {
        status: 'error',
        message: error instanceof Error ? error.message : 'Unknown error',
      }
    }
  },

  /**
   * Add a symbol mapping to a strategy
   * Note: In desktop version, this updates the strategy's symbol directly
   */
  addSymbolMapping: async (
    strategyId: number,
    data: AddSymbolRequest
  ): Promise<ApiResponse<void>> => {
    try {
      await strategyCommands.updateStrategy({
        id: strategyId,
        exchange: data.exchange,
        symbol: data.symbol,
        quantity: data.quantity,
        product: data.product_type,
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
   * Add bulk symbol mappings
   * Note: In desktop version, this is not directly supported (single symbol per strategy)
   */
  addBulkSymbols: async (
    _strategyId: number,
    _csvData: string
  ): Promise<ApiResponse<{ added: number; failed: number }>> => {
    return {
      status: 'error',
      message:
        'Bulk symbol import not supported in desktop version. Use single symbol per strategy.',
    }
  },

  /**
   * Delete a symbol mapping
   * Note: In desktop version, this clears the strategy's symbol
   */
  deleteSymbolMapping: async (
    strategyId: number,
    _mappingId: number
  ): Promise<ApiResponse<void>> => {
    try {
      await strategyCommands.updateStrategy({
        id: strategyId,
        symbol: '',
        exchange: '',
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
   * Search symbols
   */
  searchSymbols: async (query: string, exchange?: string): Promise<SymbolSearchResult[]> => {
    const results = await symbolCommands.searchSymbols(query, exchange, 50)
    // Transform results to include frontend-compatible fields
    return results.map((r) => ({
      ...r,
      brsymbol: r.brsymbol || r.symbol,
      lotsize: r.lotsize || r.lot_size,
    }))
  },

  /**
   * Get symbol info
   */
  getSymbolInfo: async (exchange: string, symbol: string): Promise<SymbolSearchResult> => {
    const result = await symbolCommands.getSymbolInfo(exchange, symbol)
    return {
      ...result,
      brsymbol: result.brsymbol || result.symbol,
      lotsize: result.lotsize || result.lot_size,
    }
  },

  /**
   * Refresh symbol master data
   */
  refreshSymbolMaster: async (): Promise<number> => {
    return symbolCommands.refreshSymbolMaster()
  },

  /**
   * Get webhook URL for a strategy
   * Note: In desktop version, webhooks are handled locally
   */
  getWebhookUrl: (webhookId: string): string => {
    // Desktop app doesn't have a public webhook URL
    // This returns a local reference for display purposes
    return `openalgo://webhook/${webhookId}`
  },
}

export default strategyApi
