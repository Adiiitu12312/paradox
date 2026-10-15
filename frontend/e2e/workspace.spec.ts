import { test, expect } from '@playwright/test'
import { execFileSync, spawn, type ChildProcess } from 'node:child_process'
import { mkdtempSync, readFileSync, writeFileSync, rmSync, realpathSync, mkdirSync, chmodSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, resolve, sep } from 'node:path'
import { DatabaseSync } from 'node:sqlite'

let root: string
let child: ChildProcess | undefined
let exited: Promise<number | null>
let url: string
let original: string
let working: string
const binary = process.env.PARADOX_BIN ? resolve(process.env.PARADOX_BIN) : ''
const env = Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith('GIT_')))
function git(...args: string[]) { return execFileSync('git', args, { cwd: root, env: { ...env, GIT_CONFIG_NOSYSTEM: '1', GIT_CONFIG_GLOBAL: join(root, 'absent-global') }, encoding: 'utf8' }) }

test.beforeEach(async () => {
  if (!binary) throw new Error('Set PARADOX_BIN to a compiled Paradox binary; tests never use a real repository.')
  root = mkdtempSync(join(tmpdir(), 'paradox-ui-e2e-'))
  expect(realpathSync(root).startsWith(realpathSync(tmpdir()) + sep)).toBe(true)
  git('init', '-q', '--initial-branch=main')
  for (const [key, value] of [['user.name', 'UI Fixture'], ['user.email', 'fixture@example.invalid'], ['core.autocrlf', 'false'], ['commit.gpgsign', 'false'], ['core.hooksPath', '.git/no-hooks']]) git('config', key, value)
  original = Array.from({ length: 40 }, (_, index) => `const value${index} = ${index};`).join('\n') + '\n'
  writeFileSync(join(root, 'sample.ts'), original)
  git('add', 'sample.ts'); git('commit', '-qm', 'Initial fixture')
  working = '//> clarify first value\n' + original.replace('value0 = 0', 'value0 = 100').replace('value39 = 39', 'value39 = 3900')
  writeFileSync(join(root, 'sample.ts'), working)
  const assets = process.env.PARADOX_ASSETS_DIR ? ['--assets-dir', resolve(process.env.PARADOX_ASSETS_DIR)] : []
  child = spawn(binary, ['ui', '--no-open', ...assets], { cwd: root, env: { ...env, GIT_CONFIG_NOSYSTEM: '1', GIT_CONFIG_GLOBAL: join(root, 'absent-global') }, stdio: ['ignore', 'pipe', 'pipe'] })
  exited = new Promise((resolveExit, reject) => { child!.once('exit', resolveExit); child!.once('error', reject) })
  url = await new Promise<string>((resolveUrl, reject) => {
    let output = ''
    const timer = setTimeout(() => reject(new Error(`UI did not start: ${output}`)), 20_000)
    child!.once('error', error => { clearTimeout(timer); reject(error) })
    child!.stdout!.on('data', chunk => { output += String(chunk); const match = output.match(/http:\/\/127\.0\.0\.1:\d+\/\?token=[a-f0-9]+/); if (match) { clearTimeout(timer); resolveUrl(match[0]) } })
    child!.stderr!.on('data', chunk => { output += String(chunk) })
  })
})
test.afterEach(async ({ context }) => {
  try {
    await context.close()
    if (child) {
      try { const code = await Promise.race([exited, new Promise((_, reject) => setTimeout(() => reject(new Error('UI process remained after last tab closed')), 8_000))]); expect(code).toBe(0) }
      finally { if (child.exitCode === null) { child.kill(); await exited }; child = undefined }
    }
  } finally {
    if (root) { expect(realpathSync(root).startsWith(realpathSync(tmpdir()) + sep)).toBe(true); rmSync(root, { recursive: true, force: true }) }
  }
})

test('stages one hunk, commits only the index, strips markers, and leaves working bytes intact', async ({ page }) => {
  const errors: string[] = []
  page.on('pageerror', error => errors.push(error.message))
  await page.goto(url)
  await expect(page).toHaveURL(/\/$/)
  await expect(page.locator('.sidebar-note')).toContainText('Local session')
  await page.locator('.file-name').filter({ hasText: 'sample.ts' }).click()
  await expect(page.locator('.monaco-diff-editor')).toBeVisible()
  await page.getByText('Select hunks or lines').click()
  await expect(page.getByRole('button', { name: 'Stage hunk', exact: true })).toHaveCount(2)
  await page.getByRole('button', { name: 'Stage hunk', exact: true }).first().click()
  await expect(page.getByRole('status')).toContainText('selection staged')
  expect(git('show', ':sample.ts')).toContain('value0 = 100')
  expect(git('show', ':sample.ts')).toContain('value39 = 39;')
  await page.getByLabel('Commit message').fill('Commit selected hunk')
  await page.getByRole('button', { name: 'Commit 1 files', exact: true }).click()
  await expect(page.getByRole('status')).toContainText('Commit created')
  expect(git('show', 'HEAD:sample.ts')).toBe(original.replace('value0 = 0', 'value0 = 100'))
  expect(readFileSync(join(root, 'sample.ts'), 'utf8')).toBe(working)
  await page.getByRole('button', { name: 'History', exact: true }).click()
  await page.getByRole('button', { name: /Commit selected hunk/ }).click()
  await expect(page.getByRole('heading', { name: 'Commit selected hunk' })).toBeVisible()
  expect(errors).toEqual([])
})

test('missing authentication and foreign origins cannot mutate the index', async ({ page, request }) => {
  await page.goto(url)
  const origin = new URL(url).origin
  expect((await request.get(`${origin}/api/status`)).status()).toBe(401)
  const response = await page.request.post(`${origin}/api/stage`, { headers: { Origin: 'https://example.invalid' }, data: { mode: 'all' } })
  expect(response.status()).toBe(403)
  expect(git('diff', '--cached')).toBe('')
  await expect(page.locator('.sidebar-note')).toContainText('Local session')
})

test('two commits in one UI session attach fallback loops to the current HEAD', async ({ page }) => {
  await page.goto(url)
  await page.getByRole('button', { name: 'Stage all', exact: true }).click()
  await expect(page.getByRole('status')).toContainText('selection staged')
  await page.getByLabel('Commit message').fill('First session commit')
  await page.getByRole('button', { name: 'Commit 1 files', exact: true }).click()
  await expect(page.getByRole('status')).toContainText('Commit created')
  const firstHead = git('rev-parse', 'HEAD').trim()
  const next = '//> second session edit\n' + original.replace('value0 = 0', 'value0 = 200')
  writeFileSync(join(root, 'sample.ts'), next)
  await page.getByRole('button', { name: 'Refresh', exact: true }).click()
  await page.getByRole('button', { name: 'Stage all', exact: true }).click()
  await expect(page.getByRole('status')).toContainText('selection staged')
  await page.getByLabel('Commit message').fill('Second session commit')
  await page.getByRole('button', { name: 'Commit 1 files', exact: true }).click()
  await expect(page.getByRole('status')).toContainText('Commit created')
  expect(git('show', 'HEAD:sample.ts')).toBe(next.split('\n').slice(1).join('\n'))
  const database = new DatabaseSync(join(root, '.paradox', 'index.sqlite3'), { readOnly: true })
  try {
    const rows = database.prepare('SELECT head_sha, message FROM loops WHERE path = ? ORDER BY id DESC').all('sample.ts')
    expect(rows.some(row => row.head_sha === firstHead && row.message === 'second session edit')).toBe(true)
  } finally { database.close() }
})

test('user hooks can reject a browser commit without losing the draft or working file', async ({ page }) => {
  const hooks = join(root, '.git', 'no-hooks')
  mkdirSync(hooks)
  const hook = join(hooks, 'pre-commit')
  writeFileSync(hook, '#!/bin/sh\necho "UI fixture hook refused commit" >&2\nexit 1\n')
  chmodSync(hook, 0o755)
  const before = git('rev-parse', 'HEAD')
  await page.goto(url)
  await expect(page.locator('.sidebar-note')).toContainText('Local session')
  await page.getByRole('button', { name: 'Stage all', exact: true }).click()
  await expect(page.getByRole('status')).toContainText('selection staged')
  await page.getByLabel('Commit message').fill('Keep this draft')
  await page.getByRole('button', { name: 'Commit 1 files', exact: true }).click()
  await expect(page.getByRole('alert')).toContainText('Git commit failed')
  await expect(page.getByLabel('Commit message')).toHaveValue('Keep this draft')
  expect(git('rev-parse', 'HEAD')).toBe(before)
  expect(readFileSync(join(root, 'sample.ts'), 'utf8')).toBe(working)
  await page.getByText('Git command log').click()
  await expect(page.locator('.command-log')).toContainText('UI fixture hook refused commit')
})

test('virtualizes and pages the native graph of a 100,000-commit repository', async ({ page }) => {
  await page.goto(url)
  await expect(page.locator('.sidebar-note')).toContainText('Local session')
  const base = git('rev-parse', 'HEAD').trim()
  const stream = Array.from({ length: 100_000 }, (_, index) => {
    const subject = `Large history ${index}`
    return `commit refs/heads/main\ncommitter UI Fixture <fixture@example.invalid> ${1_700_000_000 + index} +0000\ndata ${Buffer.byteLength(subject)}\n${subject}\n${index === 0 ? `from ${base}\n` : ''}\n`
  }).join('') + 'done\n'
  execFileSync('git', ['fast-import', '--quiet'], { cwd: root, env: { ...env, GIT_CONFIG_NOSYSTEM: '1', GIT_CONFIG_GLOBAL: join(root, 'absent-global') }, input: stream })
  await page.getByRole('button', { name: 'Refresh', exact: true }).click()
  await page.getByRole('button', { name: 'History', exact: true }).click()
  await expect(page.getByRole('button', { name: /Large history 99999/ })).toBeVisible()
  await expect(page.getByRole('button', { name: /Large history 99999/ })).toContainText('HEAD')
  expect(await page.locator('.graph-row').count()).toBeLessThan(60)
  await page.locator('.graph-scroll').evaluate(element => { element.scrollTop = element.scrollHeight })
  await expect(page.getByRole('button', { name: /Large history 99800/ })).toBeVisible()
  await page.getByRole('button', { name: 'Older', exact: true }).click()
  await expect(page.getByText('Page 2', { exact: true })).toBeVisible()
  await page.locator('.graph-scroll').evaluate(element => { element.scrollTop = 0 })
  await expect(page.getByRole('button', { name: /Large history 99799/ })).toBeVisible()
  expect(await page.locator('.graph-row').count()).toBeLessThan(60)
  await page.getByRole('button', { name: 'Newer', exact: true }).click()
  await expect(page.getByText('Page 1', { exact: true })).toBeVisible()
})

test('amend requires confirmation and preserves unstaged working contents', async ({ page }) => {
  const before = git('rev-parse', 'HEAD').trim()
  await page.goto(url)
  await expect(page.locator('.sidebar-note')).toContainText('Local session')
  await page.getByLabel('Commit message').fill('Clarify initial fixture')
  await page.getByLabel('Amend latest commit').check()
  await page.getByRole('button', { name: 'Amend commit', exact: true }).click()
  await expect(page.getByRole('dialog')).toBeVisible()
  expect(git('rev-parse', 'HEAD').trim()).toBe(before)
  await page.getByRole('dialog').getByRole('button', { name: 'Cancel', exact: true }).click()
  expect(git('rev-parse', 'HEAD').trim()).toBe(before)
  await page.getByRole('button', { name: 'Amend commit', exact: true }).click()
  await page.getByRole('dialog').getByRole('button', { name: 'Amend commit', exact: true }).click()
  await expect(page.getByRole('status')).toContainText('Commit amended')
  expect(git('rev-parse', 'HEAD').trim()).not.toBe(before)
  expect(git('rev-list', '--count', 'HEAD').trim()).toBe('1')
  expect(git('show', 'HEAD:sample.ts')).toBe(original)
  expect(git('show', `${before}:sample.ts`)).toBe(original)
  expect(readFileSync(join(root, 'sample.ts'), 'utf8')).toBe(working)
})

test('creates and switches a branch through native workflows, with cancellation preserving the repository', async ({ page }) => {
  const initialHead = git('rev-parse', 'HEAD')
  await page.goto(url)
  await page.getByRole('button', { name: 'Git workflows', exact: true }).click()
  await expect(page.getByRole('heading', { name: 'Repository workflows' })).toBeVisible()
  await page.getByLabel('Branch name', { exact: true }).fill('ui-topic')
  await page.getByRole('button', { name: 'Create branch', exact: true }).click()
  const topic = page.locator('.workflow-list > div').filter({ has: page.getByText('ui-topic', { exact: true }) })
  await expect(topic).toBeVisible()
  expect(git('rev-parse', 'refs/heads/ui-topic')).toBe(initialHead)
  await topic.getByRole('button', { name: 'Switch', exact: true }).click()
  const confirmation = page.getByRole('dialog')
  await expect(confirmation).toContainText('Confirm branch switch')
  expect(git('branch', '--show-current').trim()).toBe('main')
  await confirmation.getByRole('button', { name: 'Cancel', exact: true }).click()
  expect(git('branch', '--show-current').trim()).toBe('main')
  expect(git('rev-parse', 'HEAD')).toBe(initialHead)
  expect(readFileSync(join(root, 'sample.ts'), 'utf8')).toBe(working)
  await topic.getByRole('button', { name: 'Switch', exact: true }).click()
  await confirmation.getByRole('button', { name: 'Confirm operation', exact: true }).click()
  await expect(topic).toContainText('Current branch')
  expect(git('branch', '--show-current').trim()).toBe('ui-topic')
  expect(git('rev-parse', 'HEAD')).toBe(initialHead)
  expect(readFileSync(join(root, 'sample.ts'), 'utf8')).toBe(working)
  await expect(page.locator('.operation-progress')).toContainText('switch')
})

for (const choice of ['ours', 'theirs', 'manual'] as const) {
  test(`resolves a real native merge conflict using ${choice} and continues the merge`, async ({ page }) => {
    git('restore', 'sample.ts')
    git('checkout', '-qb', 'ui-conflict-topic')
    writeFileSync(join(root, 'sample.ts'), 'theirs from topic\n')
    git('add', 'sample.ts'); git('commit', '-qm', 'Topic conflict fixture')
    git('checkout', '-q', 'main')
    writeFileSync(join(root, 'sample.ts'), 'ours from main\n')
    git('add', 'sample.ts'); git('commit', '-qm', 'Main conflict fixture')
    await page.goto(url)
    await page.getByRole('button', { name: 'Git workflows', exact: true }).click()
    const topic = page.locator('.workflow-list > div').filter({ has: page.getByText('ui-conflict-topic', { exact: true }) })
    await topic.getByRole('button', { name: 'Merge', exact: true }).click()
    await expect(page.getByRole('dialog')).toContainText('Confirm merge')
    await page.getByRole('dialog').getByRole('button', { name: 'Confirm operation', exact: true }).click()
    await expect(page.getByRole('alert')).toContainText('Git operation exited')
    expect(git('ls-files', '--unmerged', '-z')).not.toBe('')
    expect(git('show', ':2:sample.ts')).toBe('ours from main\n')
    expect(git('show', ':3:sample.ts')).toBe('theirs from topic\n')
    await page.getByRole('button', { name: 'File history & conflicts', exact: true }).click()
    await page.getByLabel('Repository-relative file path', { exact: true }).fill('sample.ts')
    await page.getByRole('button', { name: 'Resolve conflict', exact: true }).click()
    await expect(page.getByLabel('Resolved file contents')).toHaveValue(/<<<<<<</)
    const result = choice === 'manual' ? 'manually combined result\n' : choice === 'ours' ? 'ours from main\n' : 'theirs from topic\n'
    if (choice === 'manual') {
      await page.getByLabel('Resolved file contents').fill(result)
      await page.getByRole('button', { name: 'Save result & mark resolved', exact: true }).click()
    } else {
      await page.getByRole('button', { name: `Accept ${choice} & mark resolved`, exact: true }).click()
    }
    await expect(page.locator('.conflict-columns')).toHaveCount(0)
    expect(git('ls-files', '--unmerged', '-z')).toBe('')
    expect(git('show', ':sample.ts')).toBe(result)
    expect(readFileSync(join(root, 'sample.ts'), 'utf8')).toBe(result)
    await page.getByRole('button', { name: 'Continue', exact: true }).click()
    await expect(page.locator('.git-workflows').getByRole('status')).toContainText('Git operation completed.')
    expect(git('rev-list', '--parents', '-1', 'HEAD').trim().split(/\s+/)).toHaveLength(3)
    expect(git('show', 'HEAD:sample.ts')).toBe(result)
  })
}

test('destructive cancellation preserves refs and a stale branch-deletion preview returns 409', async ({ page }) => {
  git('branch', 'ui-delete-me')
  const originalHead = git('rev-parse', 'HEAD')
  const originalRefs = git('for-each-ref', '--format=%(refname)%00%(objectname)')
  const originalIndex = git('diff', '--cached', '--binary')
  await page.goto(url)
  await page.getByRole('button', { name: 'Git workflows', exact: true }).click()
  const branch = page.locator('.workflow-list > div').filter({ has: page.getByText('ui-delete-me', { exact: true }) })
  await branch.getByRole('button', { name: 'Delete…', exact: true }).click()
  const confirmation = page.getByRole('dialog')
  await expect(confirmation).toContainText('ui-delete-me')
  await confirmation.getByRole('button', { name: 'Cancel', exact: true }).click()
  expect(git('rev-parse', 'HEAD')).toBe(originalHead)
  expect(git('for-each-ref', '--format=%(refname)%00%(objectname)')).toBe(originalRefs)
  expect(git('diff', '--cached', '--binary')).toBe(originalIndex)
  expect(readFileSync(join(root, 'sample.ts'), 'utf8')).toBe(working)
  await branch.getByRole('button', { name: 'Delete…', exact: true }).click()
  await expect(confirmation).toBeVisible()
  const externalEdit = working + 'const externalChange = true;\n'
  writeFileSync(join(root, 'sample.ts'), externalEdit)
  const rejected = page.waitForResponse(response => response.url().endsWith('/api/actions/start') && response.request().method() === 'POST')
  await confirmation.getByRole('button', { name: 'Confirm operation', exact: true }).click()
  const response = await rejected
  expect(response.status()).toBe(409)
  await expect(page.getByRole('alert')).toContainText('changed since preview')
  expect(git('rev-parse', 'HEAD')).toBe(originalHead)
  expect(git('for-each-ref', '--format=%(refname)%00%(objectname)')).toBe(originalRefs)
  expect(git('diff', '--cached', '--binary')).toBe(originalIndex)
  expect(readFileSync(join(root, 'sample.ts'), 'utf8')).toBe(externalEdit)
})
