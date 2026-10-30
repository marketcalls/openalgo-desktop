import { describe, expect, it } from 'vitest'
import { EXPIRY_TYPES } from './constants'

/**
 * C-06: the expiry choices an options node offers, word for word as the
 * web's editor lists them (frontend/src/lib/flow/constants.ts). The values
 * are the backend's VALID_EXPIRY_TYPES, which a Rust test compares.
 */
describe('EXPIRY_TYPES', () => {
  it('lists the web choices, labels and descriptions', () => {
    expect(EXPIRY_TYPES).toEqual([
      { value: 'current_week', label: 'Current Week', description: 'Nearest weekly expiry' },
      { value: 'next_week', label: 'Next Week', description: 'Second weekly expiry' },
      {
        value: 'current_month',
        label: 'Current Month',
        description: 'Last expiry of current month',
      },
      { value: 'next_month', label: 'Next Month', description: 'Last expiry of next month' },
    ])
  })
})
