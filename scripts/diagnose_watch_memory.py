"""Short macOS memory comparison; never substitutes for the 600-second gate."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import select
import sqlite3
import subprocess
import tempfile
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--candidate', action='append', required=True, metavar='NAME=PATH')
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    assert os.uname().sysname == 'Darwin', 'this diagnostic uses native macOS ps'
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    results = []
    for specification in args.candidate:
        name, path = specification.split('=', 1)
        binary = Path(path).resolve(strict=True)
        with tempfile.TemporaryDirectory(prefix='paradox-watch-memory-diagnostic-') as directory:
            root = Path(directory)
            env = {k: v for k, v in os.environ.items() if not k.startswith('GIT_')}
            env.update(GIT_CONFIG_NOSYSTEM='1', GIT_CONFIG_GLOBAL=str(root / 'absent-global'),
                       GIT_AUTHOR_NAME='Paradox Test', GIT_AUTHOR_EMAIL='test@example.invalid',
                       GIT_COMMITTER_NAME='Paradox Test', GIT_COMMITTER_EMAIL='test@example.invalid')

            def run(executable, *arguments):
                return subprocess.check_output([str(executable), *arguments], cwd=root, env=env,
                                               stderr=subprocess.PIPE, timeout=120)

            run('git', 'init', '-q', '--initial-branch=main')
            run('git', 'config', 'core.autocrlf', 'false')
            run('git', 'config', 'commit.gpgsign', 'false')
            run('git', 'config', 'core.hooksPath', 'no-hooks')
            (root / '.gitignore').write_text('ignored/\nnode_modules/\nbuild/\n.paradox/\n')
            (root / 'src').mkdir()
            for number in range(49_999):
                (root / 'src' / f'{number}.ts').write_text('base\n')
            run('git', 'add', '-A')
            run('git', 'commit', '-qm', 'base')
            assert len(run('git', 'ls-files', '-z').split(b'\0')) - 1 == 50_000
            run(binary, 'loop')
            (root / '.paradox' / 'config.toml').write_text('watch_idle_minutes = 0\n')
            with (output / f'{name}.log').open('wb') as log:
                child = subprocess.Popen([str(binary), 'watch'], cwd=root, env=env,
                                         stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=log)
                try:
                    assert select.select([child.stdout], [], [], 30)[0], 'watch start notice timed out'
                    line = child.stdout.readline()
                    log.write(line)
                    log.flush()
                    assert b'Paradox watching 50000 tracked files' in line, line
                    initial = subprocess.check_output(['ps', '-o', 'rss=', '-p', str(child.pid)], text=True)
                    started = time.monotonic()
                    time.sleep(60)
                    assert child.poll() is None, 'watch exited during idle diagnostic'
                    final = subprocess.check_output(['ps', '-o', 'rss=', '-p', str(child.pid)], text=True)
                    result = dict(candidate=name, binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(),
                                  tracked_files=50_000, idle_seconds=time.monotonic() - started,
                                  initial_rss_bytes=int(initial.strip()) * 1024,
                                  rss_bytes=int(final.strip()) * 1024)
                    results.append(result)
                    print(json.dumps(result), flush=True)
                    # Inspect mappings only after sampling RSS, so diagnostic
                    # tooling cannot change the recorded idle observation.
                    with (output / f'{name}-vmmap.txt').open('wb') as mappings:
                        try:
                            subprocess.run(['vmmap', '-summary', str(child.pid)], stdout=mappings,
                                           stderr=subprocess.STDOUT, timeout=20, check=False)
                        except (subprocess.SubprocessError, OSError) as error:
                            mappings.write(str(error).encode())
                    run(binary, 'watch', '--stop')
                    assert child.wait(timeout=15) == 0
                    log.write(child.stdout.read())
                    assert run(binary, 'watch', '--status').strip() == b'not running'
                    with sqlite3.connect(root / '.paradox' / 'index.sqlite3') as connection:
                        assert connection.execute('SELECT COUNT(*) FROM loops').fetchone()[0] == 0
                finally:
                    if child.poll() is None:
                        try:
                            run(binary, 'watch', '--stop')
                            child.wait(timeout=15)
                        except (subprocess.SubprocessError, OSError):
                            child.kill()
                            child.wait(timeout=5)
                    child.stdout.close()
    (output / 'memory-report.json').write_text(json.dumps({
        'purpose': 'diagnostic only; the unchanged 600-second acceptance gate remains required',
        'results': results,
    }, indent=2) + '\n')


if __name__ == '__main__':
    main()
