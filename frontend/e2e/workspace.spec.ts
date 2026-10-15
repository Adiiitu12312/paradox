import { test, expect } from '@playwright/test'
import { execFileSync, spawn, type ChildProcess } from 'node:child_process'
import { mkdtempSync, readFileSync, writeFileSync, rmSync, realpathSync, mkdirSync, chmodSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, resolve, sep } from 'node:path'

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
  child = spawn(binary, ['ui', '--no-open'], { cwd: root, env: { ...env, GIT_CONFIG_NOSYSTEM: '1', GIT_CONFIG_GLOBAL: join(root, 'absent-global') }, stdio: ['ignore', 'pipe', 'pipe'] })
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
  await context.close()
  if (child) {
    try { await Promise.race([exited, new Promise((_, reject) => setTimeout(() => reject(new Error('UI process remained after last tab closed')), 8_000))]) }
    finally { if (child.exitCode === null) child.kill(); child = undefined }
  }
  if (root) { expect(realpathSync(root).startsWith(realpathSync(tmpdir()) + sep)).toBe(true); rmSync(root, { recursive: true, force: true }) }
})

test('stages one hunk, commits only the index, strips markers, and leaves working bytes intact', async ({ page }) => {
  const errors: string[] = []
  page.on('pageerror', error => errors.push(error.message))
  await page.goto(url)
  await expect(page).toHaveURL(/\/$/)
  await expect(page.getByText('Local session', { exact: true })).toBeVisible()
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
  await expect(page.getByText('Local session', { exact: true })).toBeVisible()
})

test('user hooks can reject a browser commit without losing the draft or working file', async ({ page }) => {
  const hooks = join(root, '.git', 'no-hooks')
  mkdirSync(hooks)
  const hook = join(hooks, 'pre-commit')
  writeFileSync(hook, '#!/bin/sh\necho "UI fixture hook refused commit" >&2\nexit 1\n')
  chmodSync(hook, 0o755)
  const before = git('rev-parse', 'HEAD')
  await page.goto(url)
  await expect(page.getByText('Local session', { exact: true })).toBeVisible()
  await page.getByRole('button', { name: 'Stage all', exact: true }).click()
  await expect(page.getByRole('status')).toContainText('selection staged')
  await page.getByLabel('Commit message').fill('Keep this draft')
  await page.getByRole('button', { name: 'Commit 1 files', exact: true }).click()
  await expect(page.getByRole('alert')).toContainText('UI fixture hook refused commit')
  await expect(page.getByLabel('Commit message')).toHaveValue('Keep this draft')
  expect(git('rev-parse', 'HEAD')).toBe(before)
  expect(readFileSync(join(root, 'sample.ts'), 'utf8')).toBe(working)
  await page.getByText('Git command log').click()
  await expect(page.locator('.command-log')).toContainText('UI fixture hook refused commit')
})
