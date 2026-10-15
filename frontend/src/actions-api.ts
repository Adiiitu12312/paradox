import { ApiError } from './api'
import type { Commit, GitCommand, WithCommands } from './types'

export type GitAction =
  | { action: 'branch_create'; name: string; start?: string }
  | { action: 'branch_switch'; name: string }
  | { action: 'branch_rename'; name: string; new_name: string }
  | { action: 'branch_delete'; name: string; force: boolean }
  | { action: 'merge' | 'cherry_pick' | 'revert'; revision: string }
  | { action: 'rebase'; onto: string; interactive: false }
  | { action: 'tag_create'; name: string; target?: string; message?: string }
  | { action: 'tag_delete'; name: string }
  | { action: 'stash_create'; message?: string; include_untracked: boolean }
  | { action: 'stash_apply'; oid: string; pop: boolean }
  | { action: 'stash_drop'; oid: string }
  | { action: 'fetch'; remote: string; prune: boolean }
  | { action: 'pull'; remote: string; branch: string; rebase: boolean }
  | { action: 'push'; remote: string; branch: string; force_with_lease: boolean }
  | { action: 'reset'; target: string; mode: 'soft' | 'mixed' | 'hard' }
  | { action: 'discard'; paths: string[] }
  | { action: 'continue' | 'abort'; operation: 'merge' | 'rebase' | 'cherry_pick' | 'revert' }
export type ActionsState = WithCommands & {
  branches: { name: string; sha: string; current: boolean; upstream: string | null; unmerged: boolean }[]
  tags: { name: string; sha: string }[]
  stashes: { oid: string; selector: string; subject: string }[]
  remotes: { name: string; fetch_url: string; push_url: string }[]
  operation: 'merge' | 'rebase' | 'cherry_pick' | 'revert' | null
}
export type ActionPlan = WithCommands & {
  action: GitAction; confirmation_token: string; destructive: boolean
  losses: { path?: string; reference?: string; description: string }[]
  safety: { path: string; source: string }[]
  skipped: { path: string; source: string; reason: string }[]
  reflog_hints: string[]
}
export type Progress = { type: 'command' | 'stdout' | 'stderr' | 'exit' | 'start' | 'gap' | 'completed'; argv?: string[]; text?: string; exit_code?: number; ok?: boolean; error?: string; code?: string; commands?: GitCommand[]; result?: WithCommands }
export type FileHistory = WithCommands & { commits: Commit[]; next_cursor: string | null }
export type Blame = WithCommands & { path: string; lines: { line: number; original_line: number; sha: string; author_name: string; author_email: string; time: number; summary: string; text: string }[] }
type ConflictVersion = { text: string; binary: boolean; mode: string } | null
export type Conflict = WithCommands & { path: string; token: string; base: ConflictVersion; ours: ConflictVersion; theirs: ConflictVersion; result: string; deleted: boolean; binary: boolean }
export type ResolveRequest = { path: string; token: string; choice: 'ours' | 'theirs' | 'manual'; result?: string; mark_resolved: boolean }
export function createActionsApi(record: (commands: GitCommand[]) => void) {
  async function request<T extends WithCommands>(path: string, body?: unknown, signal?: AbortSignal): Promise<T> {
    const response = await fetch(`/api/${path}`, { credentials: 'same-origin', signal, ...(body === undefined ? {} : { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) }) })
    const text = await response.text()
    let data
    try { data = JSON.parse(text) } catch { throw new ApiError(text.slice(0, 500) || `HTTP ${response.status}`, response.status, 'invalid_response') }
    if (Array.isArray(data.commands)) record(data.commands)
    if (!response.ok || data.error) throw new ApiError(data.error || 'The operation failed.', response.status, data.code || 'git_failed', data.commands)
    return data as T
  }
  return {
    state: (signal?: AbortSignal) => request<ActionsState>('actions/state', undefined, signal),
    preview: (action: GitAction) => request<ActionPlan>('actions/preview', action),
    start: (action: GitAction, confirmation_token: string) => request<WithCommands & { operation_id: string }>('actions/start', { action, confirmation_token }),
    cancel: (id: string) => request<WithCommands>(`operations/${encodeURIComponent(id)}/cancel`, {}),
    history: (path: string, cursor?: string) => request<FileHistory>(`actions/history?${new URLSearchParams({ path, ...(cursor ? { cursor } : {}) })}`),
    blame: (path: string) => request<Blame>(`actions/blame?${new URLSearchParams({ path })}`),
    conflict: (path: string) => request<Conflict>(`actions/conflict?${new URLSearchParams({ path })}`),
    resolve: (data: ResolveRequest) => request<WithCommands>('actions/resolve', data),
    events: (id: string, onEvent: (event: Progress) => void, onError: () => void) => {
      const stream = new EventSource(`/api/operations/${encodeURIComponent(id)}/events`, { withCredentials: true })
      stream.onmessage = message => {
        try {
          const event: Progress = JSON.parse(message.data)
          if (event.result?.commands) record(event.result.commands)
          else if (event.commands) record(event.commands)
          onEvent(event)
          if (event.type === 'completed') stream.close()
        } catch { stream.close(); onError() }
      }
      // EventSource reconnects with Last-Event-ID; keep the job alive through transient transport errors.
      stream.onerror = onError
      return () => stream.close()
    },
  }
}
export type ActionsApi = ReturnType<typeof createActionsApi>
