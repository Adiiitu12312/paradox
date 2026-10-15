import { DiffEditor, loader } from '@monaco-editor/react'
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
loader.config({ monaco })

export default function MonacoDiff({ diff, theme }: { diff: Diff; theme: 'light' | 'dark' }) {
  return <DiffEditor original={diff.old_text} modified={diff.new_text} originalModelPath={`before/${diff.path}`} modifiedModelPath={`after/${diff.path}`} theme={theme === 'dark' ? 'vs-dark' : 'light'} options={{ readOnly: true, originalEditable: false, automaticLayout: true, minimap: { enabled: false }, fontFamily: 'Geist Mono Variable, monospace', fontSize: 12, scrollBeyondLastLine: false, ignoreTrimWhitespace: false, renderSideBySide: true, useInlineViewWhenSpaceIsLimited: true, renderOverviewRuler: false }} />
}
