import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { MemoryRouter } from 'react-router'
import { beforeEach, describe, expect, it, vi } from 'vitest'

const tauri = vi.hoisted(() => ({
  inShell: false,
  invoke: vi.fn(async (_cmd: string): Promise<unknown> => undefined),
}))
const navigate = vi.fn()

vi.mock('@tauri-apps/api/core', () => ({
  isTauri: () => tauri.inShell,
  invoke: (cmd: string) => tauri.invoke(cmd),
}))
vi.mock('react-router', async () => {
  const actual = await vi.importActual<typeof import('react-router')>('react-router')
  return { ...actual, useNavigate: () => navigate }
})
vi.mock('@/utils/toast', () => ({
  showToast: { success: vi.fn(), error: vi.fn() },
}))

import { ResetAccountCard } from './ResetAccountCard'

function renderCard() {
  return render(
    <MemoryRouter>
      <ResetAccountCard />
    </MemoryRouter>
  )
}

describe('ResetAccountCard', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    tauri.inShell = false
  })

  it('in a browser tab explains where to reset and offers no button', () => {
    renderCard()
    expect(screen.getByText(/Lost your password and your authenticator/)).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: /reset account/i })).toBeNull()
    expect(screen.getByText(/only in the OpenAlgo Desktop window/)).toBeInTheDocument()
  })

  it('in the desktop window asks the shell and goes to setup after a reset', async () => {
    tauri.inShell = true
    tauri.invoke.mockResolvedValue({ status: 'reset', message: 'Your account was removed.' })
    renderCard()
    await userEvent.click(screen.getByRole('button', { name: /reset account/i }))
    await waitFor(() => expect(navigate).toHaveBeenCalledWith('/setup', { replace: true }))
    expect(tauri.invoke).toHaveBeenCalledWith('reset_account')
  })

  it('stays put when the trader keeps the account', async () => {
    tauri.inShell = true
    tauri.invoke.mockResolvedValue({
      status: 'cancelled',
      message: 'Your account was not changed.',
    })
    renderCard()
    await userEvent.click(screen.getByRole('button', { name: /reset account/i }))
    await waitFor(() => expect(tauri.invoke).toHaveBeenCalled())
    expect(navigate).not.toHaveBeenCalled()
  })

  it('shows the shell refusal in trader words', async () => {
    tauri.inShell = true
    tauri.invoke.mockRejectedValue({
      code: 'AUTH_ERROR',
      message: 'Reset account works only from the OpenAlgo Desktop window on this computer.',
    })
    renderCard()
    await userEvent.click(screen.getByRole('button', { name: /reset account/i }))
    expect(await screen.findByRole('alert')).toHaveTextContent(/works only from the OpenAlgo/)
    expect(navigate).not.toHaveBeenCalled()
  })
})
