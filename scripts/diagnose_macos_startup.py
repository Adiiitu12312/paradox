"""Non-gating same-host O3/Os/Oz startup comparison in an OS-temp Git repo."""
import argparse
import csv
import json
import os
from pathlib import Path
import shutil
import statistics
import subprocess
import tempfile
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--candidate", action="append", required=True, metavar="NAME=PATH")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--samples", type=int, default=201)
    parser.add_argument("--rounds", type=int, default=3)
    parser.add_argument("--warmups", type=int, default=16)
    args = parser.parse_args()
    assert args.samples >= 201 and args.rounds >= 3 and args.warmups >= 16
    args.output = args.output.resolve()
    args.output.mkdir(parents=True, exist_ok=True)
    git = shutil.which("git")
    assert git, "Git is required"
    git = str(Path(git).resolve())
    candidates = []
    report = {"purpose": "diagnostic only; does not replace or change CI gates",
              "samples_per_candidate_per_round": args.samples,
              "rounds": args.rounds, "warmups": args.warmups,
              "spawn": "os.posix_spawn + waitpid; absolute paths; stdin/stdout/stderr DEVNULL",
              "host": dict(zip(("sysname", "nodename", "release", "version", "machine"), os.uname())),
              "candidates": {}, "round_results": [], "errors": []}
    for specification in args.candidate:
        name, path = specification.split("=", 1)
        binary = Path(path).resolve()
        if not binary.is_file():
            report["errors"].append(f"{name}: missing binary {binary}")
            continue
        candidates.append((name, str(binary)))
        report["candidates"][name] = {"bytes": binary.stat().st_size}
        inspected = subprocess.run(["otool", "-L", str(binary)], capture_output=True, text=True)
        (args.output / f"{name}-libraries.txt").write_text(inspected.stdout + inspected.stderr)
    with tempfile.TemporaryDirectory(prefix="paradox-startup-diagnostic-") as temp:
        root = Path(temp)
        env = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
        env.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=str(root / "absent-global"),
                   GIT_TERMINAL_PROMPT="0", LC_ALL="C")
        subprocess.run([git, "init", "-q"], cwd=root, env=env, check=True,
                       stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        previous_cwd = Path.cwd()
        null = os.open(os.devnull, os.O_RDWR)
        actions = [(os.POSIX_SPAWN_DUP2, null, descriptor) for descriptor in (0, 1, 2)]

        def elapsed(executable, command):
            started = time.perf_counter_ns()
            pid = os.posix_spawn(executable, [executable, *command], env, file_actions=actions)
            _, status = os.waitpid(pid, 0)
            duration = time.perf_counter_ns() - started
            code = os.waitstatus_to_exitcode(status)
            if code != 0:
                raise RuntimeError(f"{executable} {command}: exit {code}")
            return duration

        try:
            os.chdir(root)
            with (args.output.resolve() / "samples.csv").open("w", newline="") as file:
                writer = csv.writer(file)
                writer.writerow(["command", "round", "sample", "candidate", "git_ns", "candidate_ns", "paired_ns"])
                for command in (["--version"], ["status", "--porcelain=v2", "-z"]):
                    for round_number in range(args.rounds):
                        active = []
                        for name, executable in candidates:
                            try:
                                for _ in range(args.warmups):
                                    elapsed(git, command)
                                    elapsed(executable, command)
                                active.append((name, executable))
                            except Exception as error:
                                report["errors"].append(str(error))
                        measured = {name: {"git": [], "candidate": [], "paired": []} for name, _ in active}
                        for sample in range(args.samples):
                            if not active:
                                break
                            offset = (sample + round_number) % len(active)
                            order = active[offset:] + active[:offset]
                            if (sample + round_number) % 2:
                                order = list(reversed(order))
                            for position, (name, executable) in enumerate(order):
                                try:
                                    if (sample + round_number + position) % 2:
                                        wrapped = elapsed(executable, command)
                                        direct = elapsed(git, command)
                                    else:
                                        direct = elapsed(git, command)
                                        wrapped = elapsed(executable, command)
                                except Exception as error:
                                    report["errors"].append(str(error))
                                    continue
                                difference = wrapped - direct
                                measured[name]["git"].append(direct)
                                measured[name]["candidate"].append(wrapped)
                                measured[name]["paired"].append(difference)
                                writer.writerow([" ".join(command), round_number + 1, sample + 1, name, direct, wrapped, difference])
                        for name, values in measured.items():
                            if not values["paired"]:
                                continue
                            result = {"command": command, "round": round_number + 1, "candidate": name,
                                      "samples": len(values["paired"]),
                                      "git_median_ms": statistics.median(values["git"]) / 1e6,
                                      "candidate_median_ms": statistics.median(values["candidate"]) / 1e6,
                                      "signed_paired_median_ms": statistics.median(values["paired"]) / 1e6}
                            result["under_unchanged_5ms_limit"] = result["signed_paired_median_ms"] < 5
                            report["round_results"].append(result)
                            print(json.dumps(result), flush=True)
                        file.flush()
            for name, executable in candidates:
                diagnostic = subprocess.run([executable, "--help"], cwd=root,
                    env={**env, "DYLD_PRINT_STATISTICS": "1"}, capture_output=True, text=True)
                (args.output.resolve() / f"{name}-dyld.txt").write_text(diagnostic.stderr)
                report["candidates"][name]["dyld_probe_exit"] = diagnostic.returncode
        finally:
            os.chdir(previous_cwd)
            os.close(null)
    report["summary"] = []
    for name, _ in candidates:
        for command in (["--version"], ["status", "--porcelain=v2", "-z"]):
            rounds = [row for row in report["round_results"] if row["candidate"] == name and row["command"] == command]
            summary = {"candidate": name, "command": command,
                       "paired_medians_ms": [row["signed_paired_median_ms"] for row in rounds],
                       "rounds_under_unchanged_limit": sum(row["under_unchanged_5ms_limit"] for row in rounds)}
            report["summary"].append(summary)
            print(json.dumps(summary), flush=True)
    (args.output / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    # Threshold exceedances are retained observations, never a reason to skip
    # other variants. Actual build/exec failures remain visible as errors.
    return 1 if report["errors"] else 0


if __name__ == "__main__":
    raise SystemExit(main())
