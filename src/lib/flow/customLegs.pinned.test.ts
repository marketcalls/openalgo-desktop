/**
 * The custom-leg editor's offset rule at its edges (parity C-04): the web's
 * OPTION_OFFSET_PATTERN with fullmatch (services/flow_node_contracts.py:16)
 * accepts ATM, ITM1-ITM50 and OTM1-OTM50 and nothing else. Each case below
 * was checked against the web's Python. customLegs.test.ts is the web's
 * file; these pin what it leaves open, so a leg the editor accepts is one
 * the run accepts too.
 */

import { describe, expect, it } from 'vitest'
import { type CustomLeg, EMPTY_CUSTOM_LEG, OFFSET_PATTERN, validateCustomLeg } from './customLegs'

const offsetLeg = (offset: string): CustomLeg => ({ ...EMPTY_CUSTOM_LEG, offset })

const ACCEPTED = ['ATM', 'ITM1', 'ITM9', 'ITM10', 'ITM49', 'ITM50', 'OTM1', 'OTM49', 'OTM50']

/** Zero, past fifty, a leading zero, lower or mixed case, a bare side, a
 * suffix on ATM, and surrounding space: each refused by the web's run. */
const REFUSED = [
  'ITM0',
  'OTM0',
  'ITM51',
  'OTM51',
  'OTM100',
  'ITM01',
  'OTM05',
  'atm',
  'otm2',
  'Itm2',
  'ITM',
  'OTM',
  'ATM1',
  'OTM-1',
  ' OTM2',
  'OTM2 ',
]

describe('OFFSET_PATTERN', () => {
  it('accepts ATM and every offset from 1 to 50 on either side', () => {
    for (const offset of ACCEPTED) {
      expect(OFFSET_PATTERN.test(offset), offset).toBe(true)
    }
  })

  it('refuses what the web refuses', () => {
    for (const offset of REFUSED) {
      expect(OFFSET_PATTERN.test(offset), offset).toBe(false)
    }
  })
})

describe('validateCustomLeg offsets', () => {
  it('lets ITM50 and OTM50 through', () => {
    expect(validateCustomLeg(offsetLeg('ITM50'), 'MARKET')).toEqual({})
    expect(validateCustomLeg(offsetLeg('OTM50'), 'MARKET')).toEqual({})
  })

  it('names the range for every refused offset, lower case included', () => {
    for (const offset of REFUSED) {
      expect(validateCustomLeg(offsetLeg(offset), 'MARKET').offset, offset).toBe(
        'Use ATM, ITM1-ITM50 or OTM1-OTM50'
      )
    }
  })
})
