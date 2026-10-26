import { describe, expect, it } from 'vitest'
import { BROKER_COUNT } from '@/lib/brokerCatalog'
import { render, screen, userEvent } from '@/test/test-utils'
import Faq from './Faq'

describe('FAQ system requirements', () => {
  it('says Raspberry Pi is not supported and points a Pi user to OpenAlgo web', async () => {
    render(<Faq />)
    await userEvent.click(screen.getByRole('button', { name: 'What are the system requirements?' }))
    const answer = await screen.findByText(/^OpenAlgo Desktop runs on Windows 10\/11/)
    expect(answer).toHaveTextContent('64-bit Linux (x64 or ARM64)')
    expect(answer).toHaveTextContent(
      'It does not support Raspberry Pi: run OpenAlgo web on your Pi instead.'
    )
  })
})

describe('FAQ security answer', () => {
  // NEW-SEC-03: the answer describes the same transport as Server Settings'
  // warning (plain http on this computer, unencrypted to other devices,
  // https only through a tunnel), not "HTTPS for all communications".
  it('says which links are encrypted and which are not', async () => {
    render(<Faq />)
    await userEvent.click(screen.getByRole('button', { name: 'How secure is OpenAlgo?' }))
    const answer = await screen.findByText(/^OpenAlgo Desktop keeps your broker credentials/)
    expect(answer).not.toHaveTextContent(/HTTPS for all communications/i)
    expect(answer).toHaveTextContent('plain http and ws on this computer (127.0.0.1)')
    expect(answer).toHaveTextContent("broker's own secure (https and wss) addresses")
    expect(answer).toHaveTextContent(
      'If you allow access from other devices in Server Settings, that traffic is not encrypted'
    )
    expect(answer).toHaveTextContent('reach OpenAlgo through a tunnel with https')
  })
})

describe('FAQ brokers', () => {
  // docs-39: the web's answer said 36 brokers and left Rupeezy out; the
  // count and the list come from the broker catalogue.
  it('gives the catalogue count and lists Rupeezy', async () => {
    render(<Faq />)
    await userEvent.click(screen.getByRole('button', { name: 'Which brokers are supported?' }))
    const answer = await screen.findByText(/^OpenAlgo supports \d+ broker plugins/)
    expect(answer).toHaveTextContent(`OpenAlgo supports ${BROKER_COUNT} broker plugins:`)
    expect(answer).toHaveTextContent('RMoney, Rupeezy, Samco')
    await userEvent.click(screen.getByRole('button', { name: 'What is OpenAlgo?' }))
    expect(
      await screen.findByText(
        new RegExp(`across ${BROKER_COUNT} broker plugins: ${BROKER_COUNT - 1} securities`)
      )
    ).toBeInTheDocument()
  })
})
