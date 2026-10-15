import { afterEach, describe, expect, it, vi } from 'vitest'
import { ApiError, createApi } from '../api'

afterEach(() => vi.unstubAllGlobals())
describe('API errors and commands', () => {
  it('keeps command evidence when a Git hook fails', async () => {
    const commands = [{ argv: ['git', 'commit', '-m', 'message'], exit_code: 1, stderr: 'hook declined' }]
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify({ error: 'hook declined', code: 'git_failed', commands }), { status: 422 })))
    const record = vi.fn()
    await expect(createApi(record).commit('message', false)).rejects.toMatchObject({ status: 422, code: 'git_failed', commands })
    expect(record).toHaveBeenCalledWith(commands)
  })
  it('reports text extractor failures without disguising them as lost connections', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response('Request body exceeds the limit', { status: 413 })))
    await expect(createApi(vi.fn()).stage({ mode: 'all' })).rejects.toBeInstanceOf(ApiError)
  })
  it('encodes exact paths and omits an absent commit parameter', async () => {
    const fetch = vi.fn().mockResolvedValue(new Response(JSON.stringify({ commands: [] })))
    vi.stubGlobal('fetch', fetch)
    await createApi(vi.fn()).diff({ path: 'src/a & ü.ts', view: 'working' })
    const params = new URLSearchParams(fetch.mock.calls[0][0].split('?')[1])
    expect(params.get('path')).toBe('src/a & ü.ts')
    expect(params.has('commit')).toBe(false)
  })
})
