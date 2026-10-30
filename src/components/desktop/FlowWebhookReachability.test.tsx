import { render, screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'
import { checkA11y } from '@/test/a11y-utils'
import { FlowWebhookReachability, isLocalWebhookUrl } from './FlowWebhookReachability'

const TOKEN = 'wf-token'

describe('isLocalWebhookUrl', () => {
  it.each([
    `http://127.0.0.1:5000/flow/webhook/${TOKEN}`,
    `http://127.0.0.2:5500/flow/webhook/${TOKEN}`,
    `http://localhost:5000/flow/webhook/${TOKEN}`,
    `http://LOCALHOST:5000/flow/webhook/${TOKEN}`,
    `http://0.0.0.0:5000/flow/webhook/${TOKEN}`,
    `http://[::1]:5000/flow/webhook/${TOKEN}`,
  ])('reads %s as this computer only', (url) => {
    expect(isLocalWebhookUrl(url)).toBe(true)
  })

  it.each([
    `https://desk.ngrok-free.app/flow/webhook/${TOKEN}`,
    `https://algo.example.com/flow/webhook/${TOKEN}`,
    // A LAN address is reachable from the LAN, which is the trader's choice.
    `http://192.168.1.20:5000/flow/webhook/${TOKEN}`,
    // A public host whose name merely contains the word is not local.
    `https://localhost.example.com/flow/webhook/${TOKEN}`,
    'not a url',
    '',
  ])('reads %s as reachable or unknown', (url) => {
    expect(isLocalWebhookUrl(url)).toBe(false)
  })
})

describe('FlowWebhookReachability', () => {
  it('says how to reach a URL on this computer from outside', async () => {
    const { container } = render(
      <FlowWebhookReachability webhookUrl={`http://127.0.0.1:5000/flow/webhook/${TOKEN}`} />
    )
    expect(screen.getByRole('alert')).toHaveTextContent('Webhook URL not accessible!')
    expect(screen.getByRole('alert')).toHaveTextContent('built-in ngrok tunnel')
    expect(screen.getByRole('alert')).toHaveTextContent('Host Server URL')
    await checkA11y(container)
  })

  it('stays out of the way of a public URL and while the URL is loading', () => {
    const { container, rerender } = render(
      <FlowWebhookReachability webhookUrl={`https://desk.ngrok-free.app/flow/webhook/${TOKEN}`} />
    )
    expect(container).toBeEmptyDOMElement()
    rerender(<FlowWebhookReachability webhookUrl={undefined} />)
    expect(container).toBeEmptyDOMElement()
  })
})
