import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { Share2Icon, ReloadIcon, StopIcon } from '@radix-ui/react-icons'
import { createActionsApi } from './actions-api'
import type { ActionPlan, ActionsApi, ActionsState, Blame, Conflict, FileHistory, GitAction, Progress } from './actions-api'
import type { GitCommand, Status } from './types'
import { displayPath, relativeDate } from './format'

const message = (error: unknown) => error instanceof Error ? error.message : 'The operation could not finish.'
export function GitWorkflows({ record, status, onChanged, onBusy, suppliedApi }: { record: (commands: GitCommand[]) => void; status: Status; onChanged: () => void; onBusy: (busy: boolean) => void; suppliedApi?: ActionsApi }) {
  const api = useMemo(() => suppliedApi ?? createActionsApi(record), [suppliedApi, record])
  const [state, setState] = useState<ActionsState | null>(null)
  const [error, setError] = useState('')
  const [notice, setNotice] = useState('')
  const [busy, setBusy] = useState(false)
  const [plan, setPlan] = useState<ActionPlan | null>(null)
  const [operation, setOperation] = useState<string | null>(null)
  const [events, setEvents] = useState<Progress[]>([])
  const [section, setSection] = useState<'branches' | 'remotes' | 'stash' | 'inspect'>('branches')
  const dialog = useRef<HTMLDialogElement>(null)
  const stream = useRef<(() => void) | null>(null)
  const refresh = useCallback(() => api.state().then(setState).catch(reason => setError(message(reason))), [api])
  useEffect(() => { const controller = new AbortController(); api.state(controller.signal).then(setState).catch(reason => { if (!controller.signal.aborted) setError(message(reason)) }); return () => controller.abort() }, [api])
  useEffect(() => () => stream.current?.(), [])
  useEffect(() => { if (plan) dialog.current?.showModal() }, [plan])
  function activity(value: boolean) { setBusy(value); onBusy(value) }
  async function start(preview: ActionPlan) {
    dialog.current?.close(); setPlan(null); setError(''); setNotice(''); activity(true)
    try {
      const job = await api.start(preview.action, preview.confirmation_token)
      setOperation(job.operation_id); setEvents([]); stream.current?.()
      stream.current = api.events(job.operation_id, event => {
        setEvents(previous => [...previous, event].slice(-128))
        if (event.type === 'completed') {
          activity(false); setOperation(null)
          if (event.ok) { setNotice('Git operation completed.'); setError('') } else setError(event.error || 'Git operation failed; inspect the command log.')
          void refresh(); onChanged()
        }
      }, () => setNotice('Progress connection interrupted; reconnecting to the running operation…'))
    } catch (reason) { setError(message(reason)); activity(false) }
  }
  async function act(action: GitAction) {
    if (busy) return
    setError(''); setNotice(''); activity(true)
    try {
      const preview = await api.preview(action)
      if (preview.destructive) { setPlan(preview); activity(false) }
      else await start(preview)
    } catch (reason) { setError(message(reason)); activity(false) }
  }
  async function cancel() {
    if (!operation) return
    try { await api.cancel(operation); setNotice('Stopping the Git operation and its child processes…') } catch (reason) { setError(message(reason)) }
  }
  return <section className="git-workflows">
    <header className="pane-heading"><div><span className="eyebrow">NATIVE GIT</span><h2>Repository workflows</h2></div><button disabled={busy} onClick={() => void refresh()}><ReloadIcon />Refresh refs</button></header>
    <nav className="workflow-tabs" aria-label="Git workflows">{(['branches', 'remotes', 'stash', 'inspect'] as const).map(name => <button key={name} aria-current={section === name ? 'page' : undefined} onClick={() => setSection(name)}>{name === 'inspect' ? 'File history & conflicts' : name[0].toUpperCase() + name.slice(1)}</button>)}</nav>
    {error && <div className="banner error" role="alert">{error}<button onClick={() => setError('')}>Dismiss</button></div>}
    {notice && <div className="banner notice" role="status">{notice}</div>}
    {state?.operation && <div className="banner notice"><strong>{state.operation.replace('_', '-')} in progress</strong><button disabled={busy} onClick={() => void act({ action: 'continue', operation: state.operation! })}>Continue</button><button disabled={busy} onClick={() => void act({ action: 'abort', operation: state.operation! })}>Abort…</button></div>}
    {!state ? <div className="loading" role="status">Reading branches, tags and remotes…</div> : <div className="workflow-body">
      {section === 'branches' && <><div className="workflow-columns"><section><h3><Share2Icon />Branches</h3><div className="workflow-list">{state.branches.map(branch => <div key={branch.name}><div className="truncate"><strong className="mono">{branch.name}</strong><p className="muted">{branch.current ? 'Current branch' : branch.unmerged ? 'Contains unmerged commits' : 'Merged into HEAD'}{branch.upstream ? ` · ${branch.upstream}` : ''}</p></div><button disabled={busy || branch.current} onClick={() => void act({ action: 'branch_switch', name: branch.name })}>Switch</button><button disabled={busy || branch.current} onClick={() => void act({ action: 'merge', revision: branch.name })}>Merge</button><button disabled={busy || branch.current} onClick={() => void act({ action: 'branch_delete', name: branch.name, force: branch.unmerged })}>Delete…</button></div>)}</div><CreateRef kind="branch" busy={busy} act={act} /></section><section><h3>Tags</h3><div className="workflow-list">{state.tags.length ? state.tags.map(tag => <div key={tag.name}><strong className="mono truncate">{tag.name}</strong><code className="muted">{tag.sha.slice(0, 7)}</code><button disabled={busy} onClick={() => void act({ action: 'tag_delete', name: tag.name })}>Delete…</button></div>) : <p className="muted">No tags in this repository.</p>}</div><CreateRef kind="tag" busy={busy} act={act} /></section></div><AdvancedActions busy={busy} act={act} /></>}
      {section === 'remotes' && <RemoteActions state={state} branch={status.repo.branch ?? ''} busy={busy} act={act} />}
      {section === 'stash' && <StashActions state={state} busy={busy} act={act} />}
      {section === 'inspect' && <FileInspector api={api} status={status} busy={busy} onChanged={() => { onChanged(); void refresh() }} report={setError} />}
    </div>}
    {(events.length > 0 || operation) && <section className="operation-progress"><header><h3>Git progress</h3>{operation && <button onClick={() => void cancel()}><StopIcon />Cancel operation</button>}</header><div role="log" aria-live="polite">{events.map((event, index) => <pre key={index} className={event.type === 'stderr' ? 'muted' : undefined}>{event.type === 'command' ? `$ ${event.argv?.map(value => JSON.stringify(value)).join(' ')}` : event.text ?? (event.type === 'exit' ? `exit ${event.exit_code}` : event.type === 'completed' ? event.ok ? 'Completed' : event.error : 'Starting…')}</pre>)}</div></section>}
    <dialog ref={dialog} className="action-confirmation" onCancel={() => setPlan(null)}><h2>Confirm {plan?.action.action.replaceAll('_', ' ')}</h2><p>Review the exact changes before proceeding.</p>{plan && <>
      <ul>{plan.losses.map((loss, index) => <li key={index}>{loss.path && <code>{displayPath(loss.path)}: </code>}{loss.reference && <code>{loss.reference}: </code>}{loss.description}</li>)}</ul>
      {plan.safety.length > 0 && <><h3>Safety loops before changing files</h3><ul>{plan.safety.map((item, index) => <li key={index}><code>{displayPath(item.path)}</code> ({item.source})</li>)}</ul></>}
      {plan.skipped.length > 0 && <div className="banner error"><h3>These contents cannot be protected by a loop</h3><ul>{plan.skipped.map((item, index) => <li key={index}><code>{displayPath(item.path)}</code> ({item.source}): {item.reason}</li>)}</ul></div>}
      {plan.reflog_hints.map(hint => <p key={hint} className="mono">{hint}</p>)}
    </>}<div className="toolbar"><button onClick={() => { dialog.current?.close(); setPlan(null) }}>Cancel</button><button className="primary" onClick={() => plan && void start(plan)}>Confirm operation</button></div></dialog>
  </section>
}

type ActionProps = { busy: boolean; act: (action: GitAction) => Promise<void> }
function CreateRef({ kind, busy, act }: ActionProps & { kind: 'branch' | 'tag' }) {
  const [name, setName] = useState(''); const [target, setTarget] = useState(''); const [annotation, setAnnotation] = useState('')
  return <form className="workflow-form" onSubmit={event => { event.preventDefault(); void act(kind === 'branch' ? { action: 'branch_create', name, ...(target ? { start: target } : {}) } : { action: 'tag_create', name, ...(target ? { target } : {}), ...(annotation ? { message: annotation } : {}) }) }}><h4>Create {kind}</h4><label>{kind === 'branch' ? 'Branch name' : 'Tag name'}<input required value={name} onChange={event => setName(event.target.value)} disabled={busy} /></label><label>Starting revision<input placeholder="HEAD" value={target} onChange={event => setTarget(event.target.value)} disabled={busy} /></label>{kind === 'tag' && <label>Annotation (optional)<input value={annotation} onChange={event => setAnnotation(event.target.value)} disabled={busy} /></label>}<button disabled={busy || !name.trim()} type="submit">Create {kind}</button></form>
}
function AdvancedActions({ busy, act }: ActionProps) {
  const [kind, setKind] = useState('merge'); const [target, setTarget] = useState(''); const [name, setName] = useState(''); const [mode, setMode] = useState<'soft' | 'mixed' | 'hard'>('mixed')
  async function submit() {
    if (kind === 'rebase') await act({ action: 'rebase', onto: target, interactive: false })
    else if (kind === 'reset') await act({ action: 'reset', target, mode })
    else if (kind === 'branch_rename') await act({ action: 'branch_rename', name, new_name: target })
    else if (kind === 'discard') await act({ action: 'discard', paths: [target] })
    else await act({ action: kind as 'merge' | 'cherry_pick' | 'revert', revision: target })
  }
  return <form className="workflow-form advanced-actions" onSubmit={event => { event.preventDefault(); void submit() }}><h3>History operations</h3><label>Operation<select value={kind} onChange={event => setKind(event.target.value)} disabled={busy}><option value="merge">Merge a revision</option><option value="rebase">Rebase onto a revision</option><option value="cherry_pick">Cherry-pick a commit</option><option value="revert">Revert a commit</option><option value="branch_rename">Rename a branch</option><option value="reset">Reset current branch</option><option value="discard">Discard changes to one file</option></select></label>{kind === 'branch_rename' && <label>Existing branch<input required value={name} onChange={event => setName(event.target.value)} disabled={busy} /></label>}<label>{kind === 'branch_rename' ? 'New branch name' : kind === 'discard' ? 'Exact repository-relative file path' : 'Revision'}<input required value={target} onChange={event => setTarget(event.target.value)} disabled={busy} /></label>{kind === 'reset' && <label>Reset mode<select value={mode} onChange={event => setMode(event.target.value as typeof mode)} disabled={busy}><option value="soft">Soft — move HEAD</option><option value="mixed">Mixed — reset index</option><option value="hard">Hard — reset index and working files</option></select></label>}<button disabled={busy || !target.trim()} type="submit">Preview operation</button><p className="muted">Interactive rebase runs in your terminal with <code>paradox rebase -i</code>.</p></form>
}
function RemoteActions({ state, branch, busy, act }: ActionProps & { state: ActionsState; branch: string }) {
  const [remote, setRemote] = useState(state.remotes[0]?.name ?? ''); const [target, setTarget] = useState(branch); const [force, setForce] = useState(false); const [rebase, setRebase] = useState(false); const [prune, setPrune] = useState(false)
  return <section><h3>Remotes</h3><div className="workflow-list">{state.remotes.map(item => <div key={item.name}><strong>{item.name}</strong><div className="truncate"><p className="mono truncate">{item.fetch_url}</p>{item.push_url !== item.fetch_url && <p className="mono truncate">Push: {item.push_url}</p>}</div></div>)}</div>{state.remotes.length ? <div className="workflow-form"><label>Remote<select value={remote} onChange={event => setRemote(event.target.value)} disabled={busy}>{state.remotes.map(item => <option key={item.name}>{item.name}</option>)}</select></label><label>Branch<input value={target} onChange={event => setTarget(event.target.value)} disabled={busy} /></label><label className="checkbox"><input type="checkbox" checked={prune} onChange={event => setPrune(event.target.checked)} disabled={busy} />Prune stale remote refs on fetch</label><label className="checkbox"><input type="checkbox" checked={rebase} onChange={event => setRebase(event.target.checked)} disabled={busy} />Rebase when pulling</label><label className="checkbox"><input type="checkbox" checked={force} onChange={event => setForce(event.target.checked)} disabled={busy} />Push with force-with-lease (requires confirmation)</label><div className="toolbar"><button disabled={busy || !remote} onClick={() => void act({ action: 'fetch', remote, prune })}>Fetch</button><button disabled={busy || !remote || !target} onClick={() => void act({ action: 'pull', remote, branch: target, rebase })}>Pull</button><button disabled={busy || !remote || !target} onClick={() => void act({ action: 'push', remote, branch: target, force_with_lease: force })}>{force ? 'Preview force-with-lease…' : 'Push'}</button></div><p className="muted">Authentication uses your existing Git credentials. If a provider needs an interactive terminal prompt, authenticate in your terminal and retry.</p></div> : <div className="empty"><h3>No remote configured</h3><p>Add a remote using <code>paradox remote add</code>, then refresh refs.</p></div>}</section>
}
function StashActions({ state, busy, act }: ActionProps & { state: ActionsState }) {
  const [text, setText] = useState(''); const [untracked, setUntracked] = useState(false)
  return <section><h3>Stashes</h3><div className="workflow-list">{state.stashes.length ? state.stashes.map(item => <div key={item.oid}><div className="truncate"><strong>{item.selector}</strong><p className="truncate">{item.subject}</p><code className="muted">{item.oid.slice(0, 12)}</code></div><button disabled={busy} onClick={() => void act({ action: 'stash_apply', oid: item.oid, pop: false })}>Apply</button><button disabled={busy} onClick={() => void act({ action: 'stash_apply', oid: item.oid, pop: true })}>Pop…</button><button disabled={busy} onClick={() => void act({ action: 'stash_drop', oid: item.oid })}>Drop…</button></div>) : <p className="muted">No stashed changes.</p>}</div><form className="workflow-form" onSubmit={event => { event.preventDefault(); void act({ action: 'stash_create', message: text || undefined, include_untracked: untracked }) }}><label>Stash message<input value={text} onChange={event => setText(event.target.value)} disabled={busy} /></label><label className="checkbox"><input type="checkbox" checked={untracked} onChange={event => setUntracked(event.target.checked)} disabled={busy} />Include untracked files</label><button disabled={busy} type="submit">Create stash…</button></form></section>
}
function FileInspector({ api, status, busy, onChanged, report }: { api: ActionsApi; status: Status; busy: boolean; onChanged: () => void; report: (error: string) => void }) {
  const [path, setPath] = useState(''); const [loadedPath, setLoadedPath] = useState(''); const [loading, setLoading] = useState(false)
  const [history, setHistory] = useState<FileHistory | null>(null); const [blame, setBlame] = useState<Blame | null>(null); const [conflict, setConflict] = useState<Conflict | null>(null); const [result, setResult] = useState(''); const [linePage, setLinePage] = useState(0); const [cursorStack, setCursorStack] = useState<(string | undefined)[]>([undefined]); const [historyPage, setHistoryPage] = useState(0)
  async function inspect(kind: 'history' | 'blame' | 'conflict', cursor?: string, page = 0) {
    if (!path || loading) return
    setLoading(true); report('')
    try {
      if (kind === 'history') { const value = await api.history(path, cursor); setHistory(value); setBlame(null); setConflict(null); setHistoryPage(page); setCursorStack(previous => page === 0 ? [undefined] : [...previous.slice(0, page), cursor]) }
      else if (kind === 'blame') { setBlame(await api.blame(path)); setHistory(null); setConflict(null); setLinePage(0) }
      else { const value = await api.conflict(path); setConflict(value); setResult(value.result); setBlame(null); setHistory(null) }
      setLoadedPath(path)
    } catch (reason) { report(message(reason)) } finally { setLoading(false) }
  }
  async function resolve(choice: 'ours' | 'theirs' | 'manual') {
    if (!conflict || loading) return
    setLoading(true); report('')
    try { await api.resolve({ path: loadedPath, token: conflict.token, choice, ...(choice === 'manual' ? { result } : {}), mark_resolved: true }); setConflict(null); onChanged() }
    catch (reason) { report(message(reason)) } finally { setLoading(false) }
  }
  return <section><h3>Inspect a file</h3><div className="workflow-form"><label>Repository-relative file path<input list="changed-file-paths" value={path} onChange={event => setPath(event.target.value)} placeholder="src/main.rs" disabled={busy || loading} /></label><datalist id="changed-file-paths">{status.files.map(file => <option key={file.path} value={file.path} />)}</datalist><div className="toolbar"><button disabled={busy || loading || !path} onClick={() => void inspect('history')}>File Git history</button><button disabled={busy || loading || !path} onClick={() => void inspect('blame')}>Blame</button><button disabled={busy || loading || !path} onClick={() => void inspect('conflict')}>Resolve conflict</button></div></div>
    {loading && <p role="status">Reading file state…</p>}
    {(history || blame || conflict) && <h4 className="mono">{displayPath(loadedPath)}</h4>}
    {history && <><div className="workflow-list">{history.commits.map(commit => <div key={commit.sha}><code>{commit.sha.slice(0, 12)}</code><div className="truncate"><strong>{commit.subject}</strong><p className="muted">{commit.author_name} · {relativeDate(commit.time)}</p></div></div>)}</div><div className="pagination"><button disabled={loading || historyPage === 0 || path !== loadedPath} onClick={() => void inspect('history', cursorStack[historyPage - 1], historyPage - 1)}>Newer file commits</button><span>Page {historyPage + 1}</span><button disabled={loading || !history.next_cursor || path !== loadedPath} onClick={() => void inspect('history', history.next_cursor!, historyPage + 1)}>Older file commits</button></div></>}
    {blame && <><div className="blame-lines">{blame.lines.slice(linePage * 200, (linePage + 1) * 200).map(line => <div key={line.line}><span title={`${line.author_name} <${line.author_email}>\n${line.summary}`}><code>{line.sha.slice(0, 8)}</code> {line.author_name}</span><span className="muted">{line.line}</span><pre>{line.text}</pre></div>)}</div><div className="pagination"><button disabled={linePage === 0} onClick={() => setLinePage(value => value - 1)}>Previous lines</button><span>{blame.lines.length} lines</span><button disabled={(linePage + 1) * 200 >= blame.lines.length} onClick={() => setLinePage(value => value + 1)}>Next lines</button></div></>}
    {conflict && <><details><summary>Common ancestor</summary><pre className="conflict-version">{conflict.base?.binary ? 'Binary contents' : conflict.base?.text ?? '(absent)'}</pre></details><div className="conflict-columns"><section><header><h3>Ours</h3><button disabled={busy || loading} onClick={() => void resolve('ours')}>Accept ours & mark resolved</button></header><pre className="conflict-version">{conflict.ours?.binary ? 'Binary contents' : conflict.ours?.text ?? '(deleted)'}</pre></section><section><header><h3>Theirs</h3><button disabled={busy || loading} onClick={() => void resolve('theirs')}>Accept theirs & mark resolved</button></header><pre className="conflict-version">{conflict.theirs?.binary ? 'Binary contents' : conflict.theirs?.text ?? '(deleted)'}</pre></section><section><header><h3>Result</h3><button disabled={busy || loading || conflict.binary} onClick={() => void resolve('manual')}>Save result & mark resolved</button></header>{conflict.binary ? <p>Choose ours or theirs for a binary conflict.</p> : <label className="result-editor">Resolved contents<textarea aria-label="Resolved file contents" spellCheck={false} value={result} onChange={event => setResult(event.target.value)} /></label>}</section></div></>}
  </section>
}
