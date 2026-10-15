export function displayPath(path: string): string {
  // These are intentionally escaped: Git filenames can contain terminal controls.
  // eslint-disable-next-line no-control-regex
  return path.replace(/[\u0000-\u001f\u007f\u202a-\u202e\u2066-\u2069]/g,
    char => char === '\n' ? '\\n' : char === '\t' ? '\\t' : `\\u{${char.codePointAt(0)!.toString(16)}}`)
}
export function shortRef(ref: string): string { return ref.replace(/^refs\/(heads|tags|remotes)\//, '') }
export function relativeDate(seconds: number): string {
  const days = Math.floor((Date.now() / 1000 - seconds) / 86400)
  if (days < 1) return new Date(seconds * 1000).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
  if (days === 1) return 'Yesterday'
  if (days < 7) return `${days} days ago`
  return new Date(seconds * 1000).toLocaleDateString([], { month: 'short', day: 'numeric' })
}
