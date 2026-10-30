/**
 * Pins parity F-525: the panels index re-exports OrderPriceFields, the same
 * component the module itself exports. The index's type-only re-exports
 * (ExecutionStatus, LogEntry) exist only at compile time; FlowEditor imports
 * them, so the type check covers them.
 */

import { describe, expect, it } from 'vitest'
import { OrderPriceFields as fromIndex } from './index'
import { OrderPriceFields } from './OrderPriceFields'

describe('F-525: the panels index re-exports OrderPriceFields', () => {
  it('hands out the OrderPriceFields component itself', () => {
    expect(fromIndex).toBeTypeOf('function')
    expect(fromIndex).toBe(OrderPriceFields)
  })
})
