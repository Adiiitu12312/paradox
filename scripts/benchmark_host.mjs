// Check a downloaded binary without installing any build toolchain.
// Every Paradox invocation uses a new OS temporary repository.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve, sep } from 'node:path';

assert(process.argv[2], 'Usage: node scripts/benchmark_host.mjs /absolute/path/to/paradox');
const binary = resolve(process.argv[2]);
const tempParent = resolve(tmpdir());
const root = mkdtempSync(join(tempParent, 'paradox-m1-'));
const env = Object.fromEntries(Object.entries(process.env).filter(([k]) => !k.startsWith('GIT_')));
Object.assign(env, {
  GIT_CONFIG_NOSYSTEM: '1', GIT_CONFIG_GLOBAL: join(root, 'absent-global-config'),
  GIT_AUTHOR_NAME: 'Paradox Test', GIT_AUTHOR_EMAIL: 'test@example.invalid',
  GIT_COMMITTER_NAME: 'Paradox Test', GIT_COMMITTER_EMAIL: 'test@example.invalid',
  GIT_AUTHOR_DATE: '2026-01-01T00:00:00Z', GIT_COMMITTER_DATE: '2026-01-01T00:00:00Z',
  GIT_TERMINAL_PROMPT: '0', LC_ALL: 'C', TERM: 'dumb',
});
function run(executable, args) {
  const started = process.hrtime.bigint();
  const result = spawnSync(executable, args, { cwd: root, env, timeout: 30000 });
  assert.ifError(result.error);
  return { ...result, ns: Number(process.hrtime.bigint() - started) };
}
function git(...args) {
  const result = run('git', args);
  assert.equal(result.status, 0, result.stderr.toString());
}
const median = values => values.toSorted((a, b) => a - b)[Math.floor(values.length / 2)];
const os = { win32: 'windows', darwin: 'macos', linux: 'linux' }[process.platform];
const row = readFileSync(new URL('../benchmarks/passthrough.csv', import.meta.url), 'utf8')
  .split(/\r?\n/).find(line => line.startsWith(`${os},`)).split(',');
const ceiling = Math.min(Number(row[2]), Number(row[1]) * 1.5 + 1);
try {
  git('init', '-q', '--initial-branch=main');
  git('config', 'core.autocrlf', 'false');
  git('config', 'core.hooksPath', join(root, 'no-hooks'));
  git('config', 'commit.gpgsign', 'false');
  writeFileSync(join(root, 'file.txt'), 'first\n');
  git('add', 'file.txt');
  git('commit', '-qm', 'first');
  writeFileSync(join(root, 'file.txt'), 'changed\n');
  for (const args of [['status', '--short'], ['log', '--graph', '--oneline'], ['diff'], ['-C', root, 'status', '--short']]) {
    const expected = run('git', args);
    const actual = run(binary, args);
    assert.equal(actual.status, expected.status);
    assert(actual.stdout.equals(expected.stdout), `stdout differs: ${args}`);
    assert(actual.stderr.equals(expected.stderr), `stderr differs: ${args}`);
    console.log(`PASS ${args.join(' ')}\n${actual.stdout.toString().trimEnd()}`);
  }
  let passed = true;
  for (const args of [['--version'], ['status', '--porcelain=v2', '-z']]) {
    const exceeded = [];
    for (let round = 0; round < 3; round++) {
    for (let n = 0; n < 16; n++) {
      for (const executable of ['git', binary]) assert.equal(run(executable, args).status, 0);
    }
    const differences = [], direct = [], wrapped = [];
    for (let n = 0; n < 201; n++) {
      const results = (n + round) % 2 ? [run(binary, args), run('git', args)].reverse() : [run('git', args), run(binary, args)];
      assert.equal(results[0].status, 0);
      assert.equal(results[1].status, 0);
      direct.push(results[0].ns); wrapped.push(results[1].ns);
      differences.push(results[1].ns - results[0].ns);
    }
    const overhead = Math.max(0, median(differences)) / 1e6;
    console.log(JSON.stringify({ args, round: round + 1, git_ms: median(direct) / 1e6, paradox_ms: median(wrapped) / 1e6, paired_overhead_ms: overhead, pairs: 201, ceiling_ms: ceiling }));
    exceeded.push(overhead > ceiling);
    }
    const failures = exceeded.filter(Boolean).length;
    console.log(JSON.stringify({ args, exceeded, failures, required_failures: 2 }));
    passed &&= failures < 2;
  }
  assert(passed, `Median paired overhead exceeded stored OS ceiling ${ceiling} ms in at least two of three consecutive rounds`);
} finally {
  assert(root.startsWith(tempParent + sep), 'Refusing to remove a path outside the temporary parent');
  rmSync(root, { recursive: true, force: true });
}
