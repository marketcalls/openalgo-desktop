/**
 * Decision 36 (a web defect): the custom-leg editor upper-cased everything
 * typed into a leg's exact-expiry box, references included, so a typed
 * `{{webhook.expiry}}` was stored as `{{WEBHOOK.EXPIRY}}` and never
 * resolved; an offset reference was upper-cased on every save the same way.
 * The text outside a reference is still upper-cased (28oct25 -> 28OCT25).
 */

import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { useState } from 'react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

vi.mock('@/api/flow', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/api/flow')>()
  return { ...actual, getOptionStrikes: vi.fn(async () => null) }
})

const { CustomLegsFields } = await import('./CustomLegsFields')

/** The last legs array the editor wrote back to the node. */
let saved: Array<Record<string, unknown>> = []

function Harness({ initial }: { initial: unknown[] }) {
  const [legs, setLegs] = useState<unknown[]>(initial)
  return (
    <CustomLegsFields
      value={legs}
      onChange={(next) => {
        saved = next as Array<Record<string, unknown>>
        setLegs(next)
      }}
      commonPriceType="MARKET"
      commonProduct="NRML"
      commonExpiryType="current_week"
      commonAction="SELL"
      commonQuantity={1}
      strangleWidth="OTM2"
      underlying="NIFTY"
    />
  )
}

function mount(initial: unknown[]) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } })
  render(
    <QueryClientProvider client={client}>
      <Harness initial={initial} />
    </QueryClientProvider>
  )
  return userEvent.setup()
}

const REFERENCED = {
  strikeMode: 'OFFSET',
  offset: '{{webhook.offset}}',
  expiry: '{{webhook.expiry}}',
  optionType: 'CE',
  action: 'BUY',
  quantity: 1,
}

beforeEach(() => {
  saved = []
})

describe('a reference typed or stored in a leg keeps its case (decision 36)', () => {
  it('stores a pasted expiry reference as typed', async () => {
    const user = mount([REFERENCED])
    const box = await screen.findByLabelText('Expiry date')
    // Replaced in one paste: an emptied box turns back into the expiry picker.
    await user.tripleClick(box)
    await user.paste('{{payload.exp}}')
    expect(saved.at(-1)?.expiry).toBe('{{payload.exp}}')
    expect(box).toHaveValue('{{payload.exp}}')
  })

  it('still upper-cases the text around a reference', async () => {
    const user = mount([REFERENCED])
    const box = await screen.findByLabelText('Expiry date')
    await user.tripleClick(box)
    await user.paste('28oct{{yy}}')
    expect(saved.at(-1)?.expiry).toBe('28OCT{{yy}}')
  })

  it('keeps a stored offset reference through any other edit', async () => {
    const user = mount([REFERENCED])
    const box = await screen.findByLabelText('Expiry date')
    await user.clear(box)
    expect(saved.at(-1)?.offset).toBe('{{webhook.offset}}')
  })
})
