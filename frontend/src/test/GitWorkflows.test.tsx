import { cleanup, render, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { GitWorkflows } from '../GitWorkflows'
import type { ActionsApi, ActionPlan, GitAction } from '../actions-api'
import type { Status } from '../types'

afterEach(cleanup)
const status: Status = { repo: { root: '/tmp/repo', name: 'repo', branch: 'main', head: 'a'.repeat(40), unborn: false }, files: [], commands: [] }
function fixture() {
  const api: ActionsApi = {
    state: vi.fn().mockResolvedValue({ branches: [{ name: 'main', current: true, unmerged: false }, { name: 'feature', current: false, unmerged: true }], tags: [], stashes: [], remotes: [{ name: 'origin', fetch_url: '/tmp/remote', push_url: '/tmp/remote' }], operation: null, commands: [] }),
    preview: vi.fn(async (action: GitAction): Promise<ActionPlan> => ({ action, confirmation_token: 'server-plan', destructive: true, losses: [{ reference: 'refs/heads/feature', description: 'One unmerged commit becomes unreachable from this branch.' }], safety: [], skipped: [{ path: 'secret.env', source: 'working', reason: 'secret match' }], reflog_hints: ['paradox reflog'], commands: [] })),
    start: vi.fn().mockResolvedValue({ operation_id: 'job', commands: [] }),
    cancel: vi.fn(), events: vi.fn(() => vi.fn()), history: vi.fn(), blame: vi.fn(), conflict: vi.fn(), resolve: vi.fn(),
  }
  const onBusy = vi.fn(); const onChanged = vi.fn()
  render(<GitWorkflows record={vi.fn()} status={status} onBusy={onBusy} onChanged={onChanged} suppliedApi={api} />)
  return { api, user: userEvent.setup(), onBusy, onChanged }
}
describe('Git workflow confirmations', () => {
  it('shows actual losses and unprotected bytes before deleting an unmerged branch; cancel executes nothing', async () => {
    const { api, user } = fixture()
    await screen.findByText('feature')
    await user.click(screen.getAllByRole('button', { name: 'Delete…' }).find(button => !button.hasAttribute('disabled'))!)
    const dialog = await screen.findByRole('dialog')
    expect(dialog).toHaveTextContent('One unmerged commit')
    expect(dialog).toHaveTextContent('secret.env')
    expect(dialog).toHaveTextContent('secret match')
    expect(api.start).not.toHaveBeenCalled()
    await user.click(within(dialog).getByRole('button', { name: 'Cancel' }))
    expect(api.start).not.toHaveBeenCalled()
  })
  it('sends the server confirmation token and preserves a stale-plan rejection', async () => {
    const { api, user } = fixture()
    vi.mocked(api.start).mockRejectedValueOnce(new Error('Repository changed; preview again'))
    await screen.findByText('feature')
    await user.click(screen.getAllByRole('button', { name: 'Delete…' }).find(button => !button.hasAttribute('disabled'))!)
    await user.click(within(await screen.findByRole('dialog')).getByRole('button', { name: 'Confirm operation' }))
    expect(api.start).toHaveBeenCalledWith({ action: 'branch_delete', name: 'feature', force: true }, 'server-plan')
    expect(await screen.findByRole('alert')).toHaveTextContent('Repository changed; preview again')
  })
  it('requests force-with-lease explicitly and subscribes to the returned operation after confirmation', async () => {
    const { api, user } = fixture()
    await screen.findByText('feature')
    await user.click(screen.getByRole('button', { name: 'Remotes' }))
    await user.click(screen.getByRole('checkbox', { name: /force-with-lease/ }))
    await user.click(screen.getByRole('button', { name: 'Preview force-with-lease…' }))
    expect(api.preview).toHaveBeenCalledWith({ action: 'push', remote: 'origin', branch: 'main', force_with_lease: true })
    await user.click(within(await screen.findByRole('dialog')).getByRole('button', { name: 'Confirm operation' }))
    await waitFor(() => expect(api.events).toHaveBeenCalledWith('job', expect.any(Function), expect.any(Function)))
    expect(screen.getByRole('button', { name: 'Cancel operation' })).toBeEnabled()
  })
})
