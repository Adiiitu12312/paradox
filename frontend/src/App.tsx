import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { useVirtualizer } from '@tanstack/react-virtual'
import { CommitIcon, FileIcon, ReloadIcon, PlusIcon, MinusIcon, SunIcon, MoonIcon, ChevronRightIcon } from '@radix-ui/react-icons'
import { ApiError, connectSession, createApi } from './api'
import { DiffPanel } from './DiffPanel'
import { displayPath, relativeDate, shortRef } from './format'
import { deriveGraph } from './graph'
import type { CommitDetail, Diff, DiffQuery, FileChange, GitCommand, GraphPage, Selection, Status, WorkspaceApi } from './types'

function errorMessage(error: unknown) { return error instanceof Error ? error.message : 'The operation could not finish.' }
function changed(value: string) { return value !== '.' && value !== ' ' && value !== '' }

export function App({ api: suppliedApi, session = true }: { api?: WorkspaceApi; session?: boolean }) {
  const [commands, setCommands] = useState<GitCommand[]>([])
  const record = useCallback((entries: GitCommand[]) => setCommands(previous => [...previous, ...entries].slice(-100)), [])
  const api = useMemo(() => suppliedApi ?? createApi(record), [suppliedApi, record])
  const [status, setStatus] = useState<Status | null>(null)
  const [graph, setGraph] = useState<GraphPage | null>(null)
  const [cursors, setCursors] = useState<(string | undefined)[]>([undefined])
  const [page, setPage] = useState(0)
  const [tab, setTab] = useState<'graph' | 'changes'>('changes')
  const [detail, setDetail] = useState<CommitDetail | null>(null)
  const [query, setQuery] = useState<DiffQuery | null>(null)
  const [diff, setDiff] = useState<Diff | null>(null)
  const [revision, setRevision] = useState(0)
  const [error, setError] = useState('')
  const [busy, setBusy] = useState(false)
  const [loading, setLoading] = useState(true)
  const [message, setMessage] = useState('')
  const [amend, setAmend] = useState(false)
  const [notice, setNotice] = useState('')
  const [connection, setConnection] = useState('connecting')
  const [theme, setTheme] = useState<'light' | 'dark'>(() => {
    const stored = localStorage.getItem('paradox-theme')
    return stored === 'light' || stored === 'dark' ? stored : matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light'
  })
  const amendDialog = useRef<HTMLDialogElement>(null)

  useEffect(() => { document.documentElement.dataset.theme = theme }, [theme])
  useEffect(() => {
    const system = matchMedia('(prefers-color-scheme: dark)')
    const change = () => { if (!localStorage.getItem('paradox-theme')) setTheme(system.matches ? 'dark' : 'light') }
    system.addEventListener('change', change)
    return () => system.removeEventListener('change', change)
  }, [])
  function toggleTheme() { const next = theme === 'dark' ? 'light' : 'dark'; localStorage.setItem('paradox-theme', next); setTheme(next) }
  useEffect(() => session ? connectSession(setConnection) : undefined, [session])
  useEffect(() => {
    const controller = new AbortController()
    Promise.all([api.status(controller.signal), api.graph(undefined, controller.signal)]).then(([nextStatus, nextGraph]) => {
      setStatus(nextStatus); setGraph(nextGraph); setLoading(false)
    }).catch(reason => { if (!controller.signal.aborted) { setError(errorMessage(reason)); setLoading(false) } })
    return () => controller.abort()
  }, [api, revision])
  useEffect(() => {
    if (!query) return
    const controller = new AbortController()
    api.diff(query, controller.signal).then(setDiff).catch(reason => { if (!controller.signal.aborted) setError(errorMessage(reason)) })
    return () => controller.abort()
  }, [api, query, revision])

  function refresh() { setError(''); setPage(0); setCursors([undefined]); setRevision(value => value + 1) }
  async function mutate(action: () => Promise<unknown>, success: string) {
    if (busy) return
    setBusy(true); setError(''); setNotice('')
    try { await action(); setNotice(success); refresh() }
    catch (reason) {
      setError(errorMessage(reason))
      if (reason instanceof ApiError && reason.code === 'stale_diff') { setDiff(null); setRevision(value => value + 1) }
    } finally { setBusy(false) }
  }
  function selectFile(path: string, view: 'working' | 'staged', commit?: string) { setDiff(null); setQuery({ path, view, ...(commit ? { commit } : {}) }) }
  function apply(selection: Selection, view = query?.view ?? 'working') { void mutate(() => view === 'staged' ? api.unstage(selection) : api.stage(selection), view === 'staged' ? 'Index updated: selection unstaged.' : 'Index updated: selection staged.') }
  async function selectCommit(sha: string) {
    setDetail(null); setQuery(null); setDiff(null); setError('')
    try { const value = await api.detail(sha); setDetail(value) } catch (reason) { setError(errorMessage(reason)) }
  }
  async function turnPage(nextPage: number) {
    const cursor = nextPage > page ? graph?.next_cursor : cursors[nextPage]
    if (nextPage < 0 || (nextPage > page && !cursor)) return
    setBusy(true)
    try { const next = await api.graph(cursor ?? undefined); setGraph(next); setPage(nextPage); setCursors(previous => [...previous.slice(0, nextPage), cursor ?? undefined]) }
    catch (reason) { setError(errorMessage(reason)) } finally { setBusy(false) }
  }
  async function commit() {
    amendDialog.current?.close()
    await mutate(async () => { await api.commit(message, amend); setMessage(''); setAmend(false); setQuery(null); setDiff(null) }, amend ? 'Commit amended. The previous commit remains available in Git’s reflog.' : 'Commit created from the index.')
  }
  const staged = status?.files.filter(file => !file.untracked && changed(file.index_status)) ?? []
  const working = status?.files.filter(file => file.untracked || changed(file.worktree_status)) ?? []

  return <div className="workspace">
    <aside className="sidebar"><a className="brand" href="/" aria-label="Paradox workspace"><svg width="27" height="27" viewBox="0 0 28 28" fill="none" aria-hidden="true"><path d="M5 22V6h10a6 6 0 0 1 0 12H9M10 22V11h5a1 1 0 0 1 0 2h-5" stroke="currentColor" strokeWidth="2" /></svg>paradox<span className="version">local</span></a>
      <div className="repo-label"><span className="eyebrow">WORKSPACE</span><strong title={status?.repo.root}>{status?.repo.name ?? 'Connecting…'}</strong><span className="mono muted truncate">{status?.repo.branch ?? (status?.repo.unborn ? 'No commits yet' : 'Detached HEAD')}</span></div>
      <nav aria-label="Workspace"><button aria-current={tab === 'changes' ? 'page' : undefined} onClick={() => { setTab('changes'); setDetail(null); setQuery(null); setDiff(null) }}><FileIcon />Changes<span className="count">{status?.files.length ?? '—'}</span></button><button aria-current={tab === 'graph' ? 'page' : undefined} onClick={() => { setTab('graph'); setQuery(null); setDiff(null) }}><CommitIcon />History</button></nav>
      <div className="sidebar-note"><span className={`status-dot ${connection === 'connected' ? 'online' : ''}`} />{connection === 'connected' ? 'Local session' : connection === 'closed' ? 'Session disconnected' : 'Connecting session'}<p>Git on your machine.<br />Your files stay here.</p></div>
    </aside>
    <main className="main"><header className="topbar"><div><span className="muted">Workspace</span><ChevronRightIcon /><h1>{tab === 'changes' ? 'Changes' : 'History'}</h1></div><div className="toolbar"><button className="icon-button" aria-label={`Switch to ${theme === 'dark' ? 'light' : 'dark'} theme`} onClick={toggleTheme}>{theme === 'dark' ? <SunIcon /> : <MoonIcon />}</button><button disabled={busy} onClick={refresh}><ReloadIcon />Refresh</button></div></header>
      {error && <div role="alert" className="banner error">{error}<button onClick={() => setError('')}>Dismiss</button></div>}
      {notice && <div role="status" className="banner notice">{notice}</div>}
      {connection === 'closed' && session && <div className="banner error">Session ended. Run <code>paradox ui</code> again to reconnect.</div>}
      {loading ? <div className="loading" role="status">Opening local repository…</div> : !status ? <div className="empty"><h2>Workspace unavailable</h2><p>Start this interface with <code>paradox ui</code> from a Git repository.</p><button onClick={refresh}>Try again</button></div> : <div className="content">
        {tab === 'changes' ? <>
          <aside className="files-pane"><header className="pane-heading"><div><span className="eyebrow">WORKING DIRECTORY</span><h2>{status.files.length} changed files</h2></div></header>
            <FileList title="Staged" files={staged} selected={query} view="staged" busy={busy} select={selectFile} apply={apply} />
            <FileList title="Unstaged" files={working} selected={query} view="working" busy={busy} select={selectFile} apply={apply} />
            {!status.files.length && <div className="empty"><h3>Working tree clean</h3><p>Your next changes will appear here. Refresh after saving.</p></div>}
            <form className="commit-form" onSubmit={event => { event.preventDefault(); if (amend) amendDialog.current?.showModal(); else void commit() }}><label htmlFor="commit-message">Commit message</label><textarea id="commit-message" rows={4} placeholder="Describe this change…" value={message} onChange={event => setMessage(event.target.value)} required disabled={busy} /><label className="checkbox"><input type="checkbox" checked={amend} disabled={busy || !status.repo.head} onChange={event => setAmend(event.target.checked)} />Amend latest commit</label><p className="muted">Commits use staged contents. Message markers are stripped from the index.</p><button className="primary" disabled={busy || !message.trim() || (!staged.length && !amend)} type="submit"><CommitIcon />{busy ? 'Working…' : amend ? 'Amend commit' : `Commit ${staged.length} files`}</button></form>
          </aside>
          <div className="viewer">{query ? diff ? <DiffPanel key={diff.token + diff.view + diff.path} diff={diff} busy={busy} theme={theme} apply={apply} /> : <div className="loading" role="status">Reading file diff…</div> : <div className="empty"><FileIcon width={28} height={28} /><h2>Review a change</h2><p>Select a file to compare its contents.<br />Stage whole files, individual hunks, or selected lines.</p><span className="shortcut-hint">Working tree → Index → Commit</span></div>}</div>
        </> : <><section className="history-pane"><header className="pane-heading"><div><span className="eyebrow">COMMIT GRAPH</span><h2>{status.repo.branch ?? 'Repository history'}</h2></div><span className="muted">Page {page + 1}</span></header><Graph key={page} graph={graph} selected={detail?.sha} select={selectCommit} /><footer className="pagination"><button disabled={busy || page === 0} onClick={() => void turnPage(page - 1)}>Newer</button><span className="muted">{graph?.commits.length ?? 0} commits</span><button disabled={busy || !graph?.next_cursor} onClick={() => void turnPage(page + 1)}>Older</button></footer></section>
          <section className="commit-detail">{detail ? <><header className="pane-heading"><div><span className="eyebrow">{detail.sha.slice(0, 12)}</span><h2>{detail.subject}</h2></div></header><div className="commit-meta"><strong>{detail.author_name}</strong><span className="muted">{detail.author_email}</span><time>{new Date(detail.time * 1000).toLocaleString()}</time><pre>{detail.message}</pre></div><div className="commit-files">{detail.files.map(file => <button key={file.path} onClick={() => selectFile(file.path, 'staged', detail.sha)}><span className="file-status">{file.status}</span><span className="truncate mono">{displayPath(file.path)}</span><span className="additions">{file.additions === null ? '' : `+${file.additions}`}</span><span className="deletions">{file.deletions === null ? '' : `−${file.deletions}`}</span></button>)}</div>{query && (diff ? <DiffPanel key={diff.token + diff.path} diff={diff} busy={busy} theme={theme} apply={apply} /> : <div className="loading">Reading commit diff…</div>)}</> : <div className="empty"><CommitIcon width={28} height={28} /><h2>Select a commit</h2><p>Inspect the message, author, and changed files.</p></div>}</section></>}
      </div>}
      <details className="command-log"><summary>Git command log <span className="muted">{commands.length} recent commands</span></summary><div>{commands.length ? commands.map((command, index) => <section key={index}><code>{command.argv.map(argument => JSON.stringify(argument)).join(' ')}</code><span className={command.exit_code ? 'deletions' : 'muted'}>exit {command.exit_code}</span>{command.stderr && <pre>{command.stderr}</pre>}</section>) : <p className="muted">Commands appear here as you work.</p>}</div></details>
    </main>
    <dialog ref={amendDialog}><h2>Amend the latest commit?</h2><p>This replaces the current HEAD commit. If it was already pushed, coordinate with collaborators before rewriting shared history.</p><div className="toolbar"><button onClick={() => amendDialog.current?.close()}>Cancel</button><button className="primary" onClick={() => void commit()}>Amend commit</button></div></dialog>
  </div>
}

function FileList({ title, files, selected, view, busy, select, apply }: { title: string; files: FileChange[]; selected: DiffQuery | null; view: 'staged' | 'working'; busy: boolean; select: (path: string, view: 'staged' | 'working') => void; apply: (selection: Selection, view: 'staged' | 'working') => void }) {
  return <section className="file-list"><header><h3>{title}<span className="count">{files.length}</span></h3><button className="text-button" disabled={busy || !files.length} onClick={() => apply({ mode: 'all' }, view)}>{view === 'staged' ? 'Unstage all' : 'Stage all'}</button></header>{files.map(file => <div className={`file-row ${selected?.path === file.path && selected.view === view ? 'selected' : ''}`} key={file.path}><button className="file-name" onClick={() => select(file.path, view)} title={displayPath(file.path)}><span className="file-status">{file.untracked ? '?' : view === 'staged' ? file.index_status : file.worktree_status}</span><span className="truncate mono">{displayPath(file.path)}</span></button><button className="icon-button" disabled={busy} aria-label={`${view === 'staged' ? 'Unstage' : 'Stage'} ${displayPath(file.path)}`} onClick={() => apply({ mode: 'files', paths: [file.path] }, view)}>{view === 'staged' ? <MinusIcon /> : <PlusIcon />}</button></div>)}</section>
}

function Graph({ graph, selected, select }: { graph: GraphPage | null; selected?: string; select: (sha: string) => void }) {
  const scroll = useRef<HTMLDivElement>(null)
  // No React Compiler is enabled; TanStack owns its imperative scroll state.
  // eslint-disable-next-line react-hooks/incompatible-library
  const virtual = useVirtualizer({ count: graph?.commits.length ?? 0, getScrollElement: () => scroll.current, estimateSize: () => 66, overscan: 10 })
  const layout = useMemo(() => deriveGraph(graph?.commits ?? []), [graph])
  const graphWidth = Math.max(48, layout.width)
  return <div className="graph-scroll" ref={scroll} role="region" tabIndex={0} aria-label="Commit history">{!graph?.commits.length ? <div className="empty"><h3>No commits yet</h3><p>Create your first commit from Changes.</p></div> : <div style={{ height: virtual.getTotalSize(), minWidth: graphWidth + 350, position: 'relative' }}>{virtual.getVirtualItems().map(row => { const commit = graph.commits[row.index]; const node = layout.rows[row.index]; return <button key={commit.sha} className={`graph-row ${selected === commit.sha ? 'selected' : ''}`} style={{ height: row.size, transform: `translateY(${row.start}px)` }} onClick={() => select(commit.sha)}><svg className="graph-lines" width={graphWidth} height="66" aria-hidden="true">{node.edges.filter(edge => edge.to >= 0).map((edge, index) => <path key={index} d={`M${12 + edge.from * 13} ${edge.from === node.lane ? 33 : 0} C${12 + edge.from * 13} 49, ${12 + edge.to * 13} 49, ${12 + edge.to * 13} 66`} fill="none" stroke="var(--accent)" strokeWidth="1.4" />)}{node.incoming && <path d={`M${12 + node.lane * 13} 0V33`} stroke="var(--accent)" strokeWidth="1.4" />}<circle cx={12 + node.lane * 13} cy="33" r="4" fill={commit.sha === graph.head ? 'var(--accent)' : 'var(--bg)'} stroke="var(--accent)" strokeWidth="2" /></svg><span className="graph-description"><span className="truncate">{commit.subject || '(no subject)'}</span><span className="muted graph-subtitle"><code>{commit.sha.slice(0, 7)}</code><span className="truncate">{commit.author_name}</span><time>{relativeDate(commit.time)}</time></span></span>{commit.sha === graph.head && <span className="head-badge" title="Current HEAD">HEAD</span>}{commit.refs.length > 0 && <span className="ref-badge" title={commit.refs.join('\n')}>{shortRef(commit.refs[0])}{commit.refs.length > 1 ? ` +${commit.refs.length - 1}` : ''}</span>}</button> })}</div>}</div>
}
