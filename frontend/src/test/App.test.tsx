import { cleanup, render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { App } from '../App'
import type { WorkspaceApi } from '../types'

afterEach(cleanup)
function fixture(): WorkspaceApi {
  return {
    status: vi.fn().mockResolvedValue({ repo: { root: '/tmp/repo', name: 'test-repo', branch: 'main', head: 'a'.repeat(40), unborn: false }, files: [{ path: 'hello\nü.ts', old_path: null, index_status: 'M', worktree_status: 'M', untracked: false }], commands: [] }),
    graph: vi.fn().mockResolvedValue({ commits: [], next_cursor: null, head: 'a'.repeat(40), commands: [] }),
    detail: vi.fn(), diff: vi.fn(),
    stage: vi.fn().mockResolvedValue({ changed: true, commands: [] }),
    unstage: vi.fn().mockResolvedValue({ changed: true, commands: [] }),
    commit: vi.fn().mockResolvedValue({ head: 'b'.repeat(40), commands: [] }),
  }
}
describe('Workspace index actions', () => {
  it('displays escaped paths but stages the original exact Git filename', async () => {
    const api = fixture(); const user = userEvent.setup()
    render(<App api={api} session={false} />)
    await user.click(await screen.findByRole('button', { name: 'Stage hello\\nü.ts' }))
    expect(api.stage).toHaveBeenCalledWith({ mode: 'files', paths: ['hello\nü.ts'] })
    expect(await screen.findByRole('status')).toHaveTextContent('selection staged')
  })
  it('commits an explicit message without silently auto-staging working files', async () => {
    const api = fixture(); const user = userEvent.setup()
    render(<App api={api} session={false} />)
    await user.type(await screen.findByLabelText('Commit message'), 'Fix parser')
    await user.click(screen.getByRole('button', { name: 'Commit 1 files' }))
    await waitFor(() => expect(api.commit).toHaveBeenCalledWith('Fix parser', false))
    expect(api.stage).not.toHaveBeenCalled()
    expect(api.unstage).not.toHaveBeenCalled()
  })
  it('shows actionable API failures and keeps the draft message', async () => {
    const api = fixture(); const user = userEvent.setup()
    vi.mocked(api.commit).mockRejectedValue(new Error('pre-commit hook rejected this commit'))
    render(<App api={api} session={false} />)
    await user.type(await screen.findByLabelText('Commit message'), 'Keep my draft')
    await user.click(screen.getByRole('button', { name: 'Commit 1 files' }))
    expect(await screen.findByRole('alert')).toHaveTextContent('pre-commit hook rejected')
    expect(screen.getByLabelText('Commit message')).toHaveValue('Keep my draft')
  })
})
