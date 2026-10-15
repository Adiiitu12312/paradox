import type { GitCommand, WithCommands, WorkspaceApi } from './types'

export class ApiError extends Error {
  constructor(message: string, public status: number, public code: string, public commands: GitCommand[] = []) {
    super(message)
    this.name = 'ApiError'
  }
}

export function createApi(record: (commands: GitCommand[]) => void): WorkspaceApi {
  async function request<T extends WithCommands>(path: string, options: RequestInit = {}): Promise<T> {
    const response = await fetch(`/api/${path}`, { ...options, credentials: 'same-origin' })
    const body = await response.text()
    const data = (() => {
      try { return JSON.parse(body) }
      catch { return { error: body.trim().slice(0, 500) || `The server returned HTTP ${response.status}.`, code: 'invalid_response', commands: [] } }
    })()
    if (Array.isArray(data.commands)) record(data.commands)
    if (!response.ok || data.error) throw new ApiError(data.error ?? 'The operation failed.', response.status, data.code ?? 'git_failed', data.commands)
    return data as T
  }
  const post = <T extends WithCommands>(path: string, data: unknown) => request<T>(path, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(data) })
  return {
    status: signal => request('status', { signal }),
    graph: (cursor, signal) => request(`graph${cursor ? `?cursor=${encodeURIComponent(cursor)}` : ''}`, { signal }),
    detail: (sha, signal) => request(`commit/${encodeURIComponent(sha)}`, { signal }),
    diff: (query, signal) => {
      const params = new URLSearchParams({ path: query.path, view: query.view })
      if (query.commit) params.set('commit', query.commit)
      return request(`diff?${params}`, { signal })
    },
    stage: selection => post('stage', selection),
    unstage: selection => post('unstage', selection),
    commit: (message, amend) => post('commit', { message, amend }),
  }
}

export function connectSession(onState: (state: 'connected' | 'closed') => void): () => void {
  const socket = new WebSocket(`${location.protocol === 'https:' ? 'wss' : 'ws'}://${location.host}/api/session`)
  socket.addEventListener('open', () => onState('connected'))
  socket.addEventListener('close', () => onState('closed'))
  socket.addEventListener('error', () => onState('closed'))
  return () => socket.close(1000, 'tab closed')
}
