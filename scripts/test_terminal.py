"""Exercise real terminal inheritance on Unix, only inside a fresh temp repo."""

import argparse
import errno
import json
import os
from pathlib import Path
import select
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
import time


def terminal(executable, arguments, cwd, env, interrupt=False):
    if os.name == "nt":
        return windows_terminal(executable, arguments, cwd, env, interrupt)
    import pty

    pid, fd = pty.fork()
    if pid == 0:
        os.chdir(cwd)
        os.execvpe(executable, [executable, *arguments], env)
    output = bytearray()
    status = None
    sent_interrupt = False
    deadline = time.monotonic() + 15
    try:
        while time.monotonic() < deadline:
            if select.select([fd], [], [], 0.05)[0]:
                try:
                    data = os.read(fd, 65536)
                except OSError as error:
                    if error.errno != errno.EIO:
                        raise
                    data = b""
                output.extend(data)
                if interrupt and b"SIGNAL_READY" in output and not sent_interrupt:
                    os.write(fd, b"\x03")
                    sent_interrupt = True
                if not data:
                    _, status = os.waitpid(pid, 0)
                    break
        assert status is not None, f"terminal command timed out: {arguments}: {output!r}"
        if interrupt:
            assert sent_interrupt, output
        return os.waitstatus_to_exitcode(status), bytes(output)
    finally:
        if status is None:
            try:
                os.killpg(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            os.waitpid(pid, 0)
        os.close(fd)


def windows_terminal(executable, arguments, cwd, env, interrupt):
    # This helper is installed only on GitHub's hosted Windows runner.
    from winpty import PtyProcess

    process = PtyProcess.spawn([executable, *arguments], cwd=str(cwd), env=env)
    output = bytearray()
    sent_interrupt = False
    interrupted_at = None
    deadline = time.monotonic() + 15
    try:
        while time.monotonic() < deadline:
            if select.select([process.fileobj], [], [], 0.05)[0]:
                try:
                    output.extend(process.read(65536).encode("utf-8"))
                except EOFError:
                    break
                if interrupt and b"SIGNAL_READY" in output and not sent_interrupt:
                    process.sendintr()
                    sent_interrupt = True
                    interrupted_at = time.monotonic()
            elif not process.isalive():
                break
        assert not process.isalive(), f"terminal command timed out: {arguments}: {output!r}"
        if interrupt:
            assert sent_interrupt, output
            assert time.monotonic() - interrupted_at < 5, "Ctrl+C did not end the command promptly"
        return process.exitstatus, bytes(output)
    finally:
        process.close(force=True)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("binary", type=Path)
    args = parser.parse_args()
    binary = str(args.binary.resolve(strict=True))
    git = shutil.which("git")
    assert git
    with tempfile.TemporaryDirectory(prefix="paradox-terminal-") as temp:
        root = Path(temp)
        env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
        env.update(
            GIT_CONFIG_NOSYSTEM="1",
            GIT_CONFIG_GLOBAL=str(root / "absent-global-config"),
            GIT_AUTHOR_NAME="Paradox Test",
            GIT_AUTHOR_EMAIL="test@example.invalid",
            GIT_COMMITTER_NAME="Paradox Test",
            GIT_COMMITTER_EMAIL="test@example.invalid",
            GIT_AUTHOR_DATE="2026-01-01T00:00:00Z",
            GIT_COMMITTER_DATE="2026-01-01T00:00:00Z",
            GIT_TERMINAL_PROMPT="0",
            TERM="xterm",
            LC_ALL="C",
        )

        def run(*arguments):
            subprocess.run([git, *arguments], cwd=root, env=env, check=True,
                           stdout=subprocess.PIPE, stderr=subprocess.PIPE)

        run("init", "-q", "--initial-branch=main")
        run("config", "core.hooksPath", str(root / "no-hooks"))
        run("config", "commit.gpgsign", "false")
        run("config", "core.autocrlf", "false")
        for text in ["first\n", "second\n"]:
            (root / "file.txt").write_text(text)
            run("add", "file.txt")
            run("commit", "-qm", text.strip())
        (root / "file.txt").write_text("modified\n")
        command = ["-c", "color.ui=auto", "status", "--short"]
        direct = terminal(git, command, root, env)
        wrapped = terminal(binary, command, root, env)
        assert direct == wrapped, (direct, wrapped)
        assert b"\x1b[" in wrapped[1], wrapped
        print("PASS: real TTY detection and automatic colors")
        run("restore", "file.txt")

        pager = root / "pager.sh"
        pager.write_bytes(b"#!/bin/sh\ntest -t 1 || exit 91\nprintf 'PAGER_TTY_OK\\n'\ncat\n")
        pager.chmod(0o755)
        paged_env = {**env, "GIT_PAGER": shlex.quote(pager.as_posix())}
        command = ["--paginate", "log", "--oneline"]
        direct = terminal(git, command, root, paged_env)
        wrapped = terminal(binary, command, root, paged_env)
        assert direct == wrapped and wrapped[0] == 0, (direct, wrapped)
        assert b"PAGER_TTY_OK" in wrapped[1], wrapped
        print("PASS: Git launches its pager with a terminal")

        editor = root / "editor.sh"
        editor.write_bytes(b"#!/bin/sh\ntest -t 0 && test -t 1 || exit 92\nprintf 'EDITOR_TTY_OK\\n'\n")
        editor.chmod(0o755)
        edited_env = {**env, "GIT_SEQUENCE_EDITOR": shlex.quote(editor.as_posix())}
        for executable in [git, binary]:
            result = terminal(executable, ["rebase", "-i", "HEAD~1"], root, edited_env)
            assert result[0] == 0 and b"EDITOR_TTY_OK" in result[1], result
        print("PASS: interactive rebase launches the sequence editor with terminal input/output")

        # Announce readiness from the long-running child itself. Printing from
        # the shell before exec races Ctrl+C against the child's startup.
        signal_child = root / "signal_child.py"
        signal_child.write_text(
            "import time\nprint('SIGNAL_READY', flush=True)\ntime.sleep(30)\n"
        )
        child_command = (
            shlex.quote(Path(sys.executable).as_posix()) + " "
            + shlex.quote(signal_child.as_posix())
        )
        command = ["-c", "alias.signal-test=!" + child_command, "signal-test"]
        results = []
        for name, executable in [("direct", git), ("wrapped", binary)]:
            trace = root / f"signal-{name}.json"
            signal_env = {**env, "GIT_TRACE2_EVENT": trace.as_posix()}
            result = terminal(executable, command, root, signal_env, interrupt=True)
            if os.name == "nt":
                # Windows shell-alias interrupt statuses vary between runs.
                # Compare each result with its own Git child's exit instead of
                # assuming two executions will choose the same status.
                # Exit events: https://git-scm.com/docs/api-trace2
                events = [json.loads(line) for line in trace.read_text().splitlines()]
                sid = next(event["sid"] for event in events if event["event"] == "start")
                codes = [event["code"] for event in events
                         if event["sid"] == sid and event["event"] in ("exit", "atexit")]
                assert codes, (name, result, events)
                assert result[0] & 0xffffffff == codes[-1] & 0xffffffff, (name, result, codes)
            results.append(result)
        if os.name != "nt":
            assert results[0][0] == results[1][0], results
        print(f"PASS: prompt Ctrl+C exit and Git status forwarding ({results[0][0]}, {results[1][0]})")

        for text in ["//> first loop\nfirst loop body\n", "//> second loop\nsecond loop body\n"]:
            (root / "file.txt").write_text(text)
            subprocess.run([binary, "loop", "file.txt"], cwd=root, env=env, check=True,
                           stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        compared = terminal(binary, ["compare", "file.txt", "HEAD", "working"], root, env)
        assert compared[0] == 0 and b"\x1b[32m" in compared[1] and b"\x1b[31m" in compared[1], compared
        print("PASS: Paradox compare colors additions and removals on a real terminal")


if __name__ == "__main__":
    main()
