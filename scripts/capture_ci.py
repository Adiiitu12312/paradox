"""Run the unchanged precompiled release capture gates once on a fresh host."""
import argparse
import json
import os
from pathlib import Path
import subprocess

from passthrough_ci import digest, require


def run(pending_test, capture_test, binary, reportdir):
    paths = {name: Path(path).resolve() for name, path in (
        ("pending", pending_test), ("capture", capture_test), ("binary", binary))}
    directory = Path(reportdir).resolve()
    directory.mkdir(parents=True, exist_ok=True)
    require(not any(directory.iterdir()), "refuse to overwrite capture assessment files")
    before = {name: digest(path) for name, path in paths.items()}
    keys = {"source": "GITHUB_SHA", "runner_os": "RUNNER_OS", "runner_arch": "RUNNER_ARCH",
            "image_os": "ImageOS", "image_version": "ImageVersion",
            "workflow_run": "GITHUB_RUN_ID", "workflow_attempt": "GITHUB_RUN_ATTEMPT"}
    report = dict(schema=1, **{name: os.environ.get(key) for name, key in keys.items()})
    errors = [f"missing provenance: {name}" for name in keys if not report[name]]
    try:
        report["git_version"] = subprocess.check_output(
            ["git", "--version"], text=True, timeout=30).strip()
    except (OSError, subprocess.SubprocessError) as error:
        report["git_version"] = None
        errors.append(f"cannot record native Git version: {error}")
    environment = dict(os.environ, PARADOX_BENCH_BINARY=str(paths["binary"]))
    assessments = []
    for name, exact, ignored, timeout in (
            ("pending", "twenty_contended_savers_queue_paths_and_holder_drains_exact_final_content", False, 300),
            ("capture", "fifty_thousand_files_single_save_and_warm_scan", True, 600)):
        command = [str(paths[name]), "--nocapture", "--exact", exact]
        if ignored:
            command.append("--ignored")
        item = dict(name=name, command=command, returncode=None, error=None)
        stdout = directory / f"{name}.stdout.log"
        stderr = directory / f"{name}.stderr.log"
        with stdout.open("w", encoding="utf-8") as out, stderr.open("w", encoding="utf-8") as err:
            try:
                result = subprocess.run(command, env=environment, stdout=out, stderr=err,
                                        timeout=timeout, check=False)
                item["returncode"] = result.returncode
            except (OSError, subprocess.SubprocessError) as error:
                item["error"] = str(error)
        assessments.append(item)
        if item["returncode"] == 0 and "test result: ok. 1 passed;" not in stdout.read_text(encoding="utf-8", errors="replace"):
            item["error"] = "exact Rust gate did not report one passing test"
        for log in (stdout, stderr):
            print(f"--- {log.name} ---", flush=True)
            print(log.read_text(encoding="utf-8", errors="replace"), flush=True)
        if item["error"] or item["returncode"] != 0:
            errors.append(f"{name} gate failed: exit={item['returncode']}, error={item['error']}")
    after = {}
    for name, path in paths.items():
        try:
            after[name] = digest(path)
        except OSError as error:
            after[name] = None
            errors.append(f"cannot recheck {name} executable: {error}")
        if after[name] != before[name]:
            errors.append(f"{name} executable changed during measurement")
    report.update(executables={name: {"path": str(path), "sha256_before": before[name],
                                    "sha256_after": after[name]} for name, path in paths.items()},
                  assessments=assessments, errors=errors, passed=not errors)
    (directory / "report.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    for error in errors:
        print(error, flush=True)
    return 1 if errors else 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("pending_test", "capture_test", "binary", "reportdir"):
        parser.add_argument(name)
    args = parser.parse_args()
    return run(args.pending_test, args.capture_test, args.binary, args.reportdir)


if __name__ == "__main__":
    raise SystemExit(main())
