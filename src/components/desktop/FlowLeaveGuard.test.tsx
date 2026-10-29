/**
 * The browser's Back with unsaved changes (parity F-170, lead follow-up
 * 2026-10-30), on the app's real router (BrowserRouter over jsdom's
 * history): a Back asks first; Stay keeps the editor and its edits; Leave
 * goes back for real; with nothing unsaved, or once saved, Back just goes
 * back. The editor's other ways out are pinned in pages/flow/FlowEditor.test.tsx.
 */

import { act, render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { BrowserRouter, Route, Routes } from 'react-router'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { useFlowWorkflowStore } from '@/stores/flowWorkflowStore'
import { FlowLeaveGuard } from './FlowLeaveGuard'

function EditorStub() {
  return (
    <>
      <p>editor page</p>
      <FlowLeaveGuard />
    </>
  )
}

function renderApp() {
  // The list, then the editor, as a trader opens a workflow.
  window.history.replaceState(null, '', '/flow')
  window.history.pushState(null, '', '/flow/editor/7')
  return render(
    <BrowserRouter>
      <Routes>
        <Route path="/flow" element={<p>workflow list</p>} />
        <Route path="/flow/editor/:id" element={<EditorStub />} />
      </Routes>
    </BrowserRouter>
  )
}

const setUnsaved = (isModified: boolean) =>
  act(() => {
    useFlowWorkflowStore.setState({ isModified })
  })

const onSentinel = () =>
  Boolean((window.history.state as Record<string, unknown> | null)?.flowLeaveGuard)

const back = () =>
  act(() => {
    window.history.back()
  })

beforeEach(() => useFlowWorkflowStore.getState().resetWorkflow())
afterEach(() => useFlowWorkflowStore.getState().resetWorkflow())

describe('the browser Back with unsaved changes', () => {
  it('asks first; Stay keeps the editor, Leave goes back for real', async () => {
    renderApp()
    expect(screen.getByText('editor page')).toBeInTheDocument()
    setUnsaved(true)
    expect(onSentinel()).toBe(true)

    await back()
    expect(await screen.findByText('Leave without saving?')).toBeInTheDocument()
    expect(screen.getByText('editor page')).toBeInTheDocument()
    expect(window.location.pathname).toBe('/flow/editor/7')

    const user = userEvent.setup()
    await user.click(screen.getByRole('button', { name: 'Stay' }))
    await waitFor(() => expect(screen.queryByText('Leave without saving?')).not.toBeInTheDocument())
    expect(onSentinel()).toBe(true)
    expect(useFlowWorkflowStore.getState().isModified).toBe(true)

    await back()
    await user.click(await screen.findByRole('button', { name: 'Leave without saving' }))
    expect(await screen.findByText('workflow list')).toBeInTheDocument()
    expect(window.location.pathname).toBe('/flow')
  })

  it('goes straight back when nothing is unsaved', async () => {
    renderApp()
    expect(onSentinel()).toBe(false)
    await back()
    expect(await screen.findByText('workflow list')).toBeInTheDocument()
    expect(screen.queryByText('Leave without saving?')).not.toBeInTheDocument()
  })

  it('takes the sentinel off once saved, so one Back leaves', async () => {
    renderApp()
    setUnsaved(true)
    expect(onSentinel()).toBe(true)
    setUnsaved(false)
    await waitFor(() => expect(onSentinel()).toBe(false))
    expect(screen.getByText('editor page')).toBeInTheDocument()
    expect(screen.queryByText('Leave without saving?')).not.toBeInTheDocument()

    await back()
    expect(await screen.findByText('workflow list')).toBeInTheDocument()
    expect(screen.queryByText('Leave without saving?')).not.toBeInTheDocument()
  })
})
