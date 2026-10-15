import { lazy, Suspense, useState } from 'react'
import { displayPath } from './format'
import type { Diff, Selection } from './types'

const CodeDiff = lazy(() => import('./MonacoDiff'))

export function DiffPanel({ diff, busy, theme, apply }: { diff: Diff; busy: boolean; theme: 'light' | 'dark'; apply: (selection: Selection) => void }) {
  const [selected, setSelected] = useState<string[]>([])
  const editable = !diff.commit
  const action = diff.view === 'staged' ? 'Unstage' : 'Stage'
  function toggle(id: string) { setSelected(previous => previous.includes(id) ? previous.filter(value => value !== id) : [...previous, id]) }
  return <section className="diff-panel" aria-label="File diff">
    <header className="pane-heading"><div className="min-w-0"><span className="eyebrow">{diff.commit ? 'Commit diff' : diff.view === 'staged' ? 'HEAD → index' : 'Index → working tree'}</span><h2 className="truncate mono" title={displayPath(diff.path)}>{displayPath(diff.path)}</h2></div>
      {editable && <button disabled={busy} onClick={() => apply({ mode: 'files', paths: [diff.path] })}>{action} file</button>}</header>
    {diff.binary || diff.truncated ? <div className="empty"><h3>{diff.binary ? 'Binary file' : 'File exceeds the preview limit'}</h3><p>Use the file action to stage or unstage the complete file.</p></div> : <>
      <div className="code-diff"><Suspense fallback={<div className="loading">Loading code editor…</div>}><CodeDiff diff={diff} theme={theme} /></Suspense></div>
      {editable && diff.hunks.length > 0 && <details className="selection-panel"><summary>Select hunks or lines <span className="muted">{diff.hunks.length} hunks</span></summary>
        <div className="selection-toolbar"><span className="muted">Select changed lines to update only part of the index.</span><button disabled={busy || !selected.length} onClick={() => apply({ mode: 'lines', path: diff.path, token: diff.token, line_ids: selected })}>{action} {selected.length} lines</button></div>
        <div className="hunks">{diff.hunks.map(hunk => <section key={hunk.id} className="hunk"><header><code>{hunk.header}</code><button disabled={busy} onClick={() => apply({ mode: 'hunks', path: diff.path, token: diff.token, hunk_ids: [hunk.id] })}>{action} hunk</button></header>
          {hunk.lines.map(line => <label key={line.id} className={`patch-line ${line.kind}`}><input type="checkbox" disabled={busy || line.kind === 'context'} checked={selected.includes(line.id)} onChange={() => toggle(line.id)} aria-label={`Select ${line.kind} line ${line.new_line ?? line.old_line}`} /><span className="line-number">{line.old_line ?? ''}</span><span className="line-number">{line.new_line ?? ''}</span><code>{line.kind === 'addition' ? '+' : line.kind === 'deletion' ? '−' : ' '} {line.text}</code></label>)}
        </section>)}</div>
      </details>}
    </>}
  </section>
}
