"""Exercise the timeline picker on native PTY/ConPTY in OS-temp repositories."""
import argparse
from concurrent.futures import ThreadPoolExecutor
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import tempfile

from test_terminal import terminal


def fixture(root):
    env = {key: value for key, value in os.environ.items() if not key.startswith('GIT_')}
    env.update(GIT_CONFIG_NOSYSTEM='1', GIT_CONFIG_GLOBAL=str(root / 'absent-global'),
               GIT_AUTHOR_NAME='Paradox Test', GIT_AUTHOR_EMAIL='test@example.invalid',
               GIT_COMMITTER_NAME='Paradox Test', GIT_COMMITTER_EMAIL='test@example.invalid',
               TERM='xterm', NO_COLOR='1')
    git = shutil.which('git')
    assert git

    def run(executable, *arguments):
        return subprocess.check_output([str(executable), *arguments], cwd=root, env=env,
                                       stderr=subprocess.PIPE, timeout=20)

    run(git, 'init', '-q', '--initial-branch=main')
    run(git, 'config', 'core.autocrlf', 'false')
    run(git, 'config', 'commit.gpgsign', 'false')
    run(git, 'config', 'core.hooksPath', str(root / 'no-hooks'))
    return env, git, run


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('binary', type=Path)
    binary = str(parser.parse_args().binary.resolve(strict=True))
    with tempfile.TemporaryDirectory(prefix='paradox-timeline-terminal-') as directory:
        root = Path(directory)
        env, git, run = fixture(root)
        (root / 'nested').mkdir()
        name = "nested/space 日本語 'quote'.ts"
        (root / name).write_text('base\n', encoding='utf-8')
        run(git, 'add', '--', name)
        run(git, 'commit', '-qm', 'base')
        (root / name).write_text('//> first loop\nfirst\n', encoding='utf-8')
        run(binary, 'loop', name)
        expected = b'//> captured while choosing\nsecond\n'
        (root / name).write_bytes(expected)

        def concurrent_saves():
            with ThreadPoolExecutor(max_workers=20) as workers:
                results = list(workers.map(lambda _: run(binary, 'loop', name), range(20)))
            assert len(results) == 20
            with sqlite3.connect(root / '.paradox/index.sqlite3') as connection:
                assert connection.execute('SELECT COUNT(*) FROM loops WHERE path=?', (name,)).fetchone()[0] == 2
                assert connection.execute('PRAGMA integrity_check').fetchone()[0] == 'ok'

        code, output = terminal(binary, ['-C', str(root / 'nested'), 'timeline'], root, env,
                                ready_marker=b'Choose a file', ready_input=b'\r',
                                on_ready=concurrent_saves, timeout_seconds=45)
        assert code == 0, (code, output)
        assert 'Loops for "nested/space 日本語'.encode() in output, output
        assert b'captured while choosing' in output and b'HEAD' in output, output
        assert (root / name).read_bytes() == expected
        print('PASS: timeline selection preserves Unicode/spaces and -C paths; 20 saves proceed while choosing')

        if os.name != 'nt':
            raw_name = 'nested/weird\n\x1b[31m.ts'
            (root / raw_name).write_text('base\n')
            run(git, 'add', '--', raw_name)
            run(git, 'commit', '-qm', 'control filename')
            (root / raw_name).write_text('//> control name\nchanged\n')
            code, output = terminal(binary, ['timeline'], root, env,
                                    ready_marker=b'Choose a file', ready_input=b'weird\r')
            assert code == 0 and b'Loops for "nested/weird\\n\\u{1b}[31m.ts"' in output, (code, output)
            with sqlite3.connect(root / '.paradox/index.sqlite3') as connection:
                assert connection.execute('SELECT COUNT(*) FROM loops WHERE path=?', (raw_name,)).fetchone()[0] == 1
            print('PASS: timeline filtering escapes terminal-control filename bytes without changing selection')

    for label, keys in [('Esc', b'\x1b'), ('Ctrl+C', b'\x03')]:
        with tempfile.TemporaryDirectory(prefix='paradox-timeline-cancel-') as directory:
            root = Path(directory)
            env, git, run = fixture(root)
            (root / 'a.ts').write_text('base\n')
            run(git, 'add', 'a.ts')
            before = (root / '.git/info/exclude').read_bytes()

            def no_store():
                assert not (root / '.paradox').exists()

            code, output = terminal(binary, ['timeline'], root, env,
                                    ready_marker=b'Choose a file', ready_input=keys, on_ready=no_store)
            assert code == 0 and b'Timeline cancelled.' in output, (label, code, output)
            assert not (root / '.paradox').exists()
            assert (root / '.git/info/exclude').read_bytes() == before
            assert (root / 'a.ts').read_bytes() == b'base\n'
            print(f'PASS: {label} cancels cleanly without creating a loop store or altering files')


if __name__ == '__main__':
    main()
