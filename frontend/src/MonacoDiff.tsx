import { useEffect, useRef } from 'react'
import * as monaco from 'monaco-editor/editor'
import EditorWorker from 'monaco-editor/editor/editor.worker?worker'
import 'monaco-editor/languages/definitions/typescript/register'
import 'monaco-editor/languages/definitions/javascript/register'
import 'monaco-editor/languages/definitions/css/register'
import 'monaco-editor/languages/definitions/html/register'
import 'monaco-editor/languages/definitions/markdown/register'
import 'monaco-editor/languages/definitions/rust/register'
import 'monaco-editor/languages/definitions/python/register'
import type { Diff } from './types'

self.MonacoEnvironment = { getWorker: () => new EditorWorker() }
let nextModel = 0

export default function MonacoDiff({ diff, theme }: { diff: Diff; theme: 'light' | 'dark' }) {
  const container = useRef<HTMLDivElement>(null)
  useEffect(() => {
    const id = ++nextModel
    const original = monaco.editor.createModel(diff.old_text, undefined, monaco.Uri.from({ scheme: 'paradox', authority: 'before', path: `/${id}/${diff.path}` }))
    const modified = monaco.editor.createModel(diff.new_text, undefined, monaco.Uri.from({ scheme: 'paradox', authority: 'after', path: `/${id}/${diff.path}` }))
    const editor = monaco.editor.createDiffEditor(container.current!, { readOnly: true, originalEditable: false, automaticLayout: true, minimap: { enabled: false }, fontFamily: 'Geist Mono Variable, monospace', fontSize: 12, scrollBeyondLastLine: false, ignoreTrimWhitespace: false, renderSideBySide: true, useInlineViewWhenSpaceIsLimited: true, renderOverviewRuler: false })
    editor.setModel({ original, modified })
    return () => {
      // Detach the view before destroying either model; pending diff work must
      // never observe disposed text models still attached to an editor.
      editor.setModel(null)
      editor.dispose()
      original.dispose()
      modified.dispose()
    }
  }, [diff])
  useEffect(() => { monaco.editor.setTheme(theme === 'dark' ? 'vs-dark' : 'light') }, [theme])
  return <div ref={container} className="h-full w-full" />
}
