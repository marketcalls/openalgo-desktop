import { describe, expect, it } from 'vitest'
import { agentErrorMessage } from './agent'

describe('agentErrorMessage', () => {
  it("shows the server's sentence when it sent one", () => {
    const error = { response: { data: { message: 'Conversation not found' } } }
    expect(agentErrorMessage(error, 'Could not open that conversation')).toBe(
      'Conversation not found'
    )
  })

  /** D-024: the web showed "Request failed with status code 409". */
  it("never shows the transport's own text", () => {
    const status = { message: 'Request failed with status code 409', response: { data: {} } }
    expect(agentErrorMessage(status, 'Could not delete that conversation')).toBe(
      'Could not delete that conversation'
    )
    const network = { message: 'Network Error' }
    expect(agentErrorMessage(network, 'Could not reach OpenAlgo')).toBe('Could not reach OpenAlgo')
  })
})
