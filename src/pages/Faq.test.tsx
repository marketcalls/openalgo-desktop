import { describe, expect, it } from 'vitest'
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
