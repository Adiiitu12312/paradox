import { describe, expect, it } from 'vitest'
import { displayPath, shortRef } from '../format'

describe('Git path display', () => {
  it('preserves Unicode while making controls and bidi overrides visible', () => {
    expect(displayPath('src/ü\n\t\u202efile.ts')).toBe('src/ü\\n\\t\\u{202e}file.ts')
    expect(displayPath('<script>.ts')).toBe('<script>.ts')
  })
  it('shortens known ref prefixes without rewriting arbitrary names', () => {
    expect(shortRef('refs/heads/main')).toBe('main')
    expect(shortRef('refs/remotes/origin/main')).toBe('origin/main')
    expect(shortRef('HEAD -> refs/heads/main')).toBe('HEAD -> refs/heads/main')
  })
})
