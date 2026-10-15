"""Record fixed fresh-host Rust assessments and validate their two-of-three vote."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import statistics
import subprocess


ROOT = Path(__file__).resolve().parent.parent
COMMANDS = {("--version",), ("status", "--porcelain=v2", "-z")}
ARCHES = {"linux": "x86_64", "macos": "aarch64", "windows": "x86_64"}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def select(build_log, destination):
    executables = []
    for line in Path(build_log).read_text(encoding="utf-8").splitlines():
        item = json.loads(line)
        if (item.get("reason") == "compiler-artifact"
                and item.get("target", {}).get("name") == "performance"
                and item.get("profile", {}).get("test")
                and item.get("executable")):
            executables.append(item["executable"])
    require(len(executables) == 1, f"expected one performance test executable: {executables}")
    destination = Path(destination)
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(executables[0], destination)


def record(test_exe, binary, report_path, attempt):
    test_exe, binary, report_path = [Path(p).resolve() for p in (test_exe, binary, report_path)]
    report_path.parent.mkdir(parents=True, exist_ok=True)
    require(not report_path.exists(), "refuse to overwrite an assessment")
    source = os.environ["GITHUB_SHA"]
    binary_hash, test_hash = digest(binary), digest(test_exe)
    environment = dict(os.environ, PARADOX_BENCH_REPORT=str(report_path),
                       PARADOX_BENCH_BINARY=str(binary), PARADOX_BENCH_ATTEMPT=str(attempt))
    command = [str(test_exe), "--ignored", "--nocapture", "--exact",
               "passthrough_overhead_within_stored_baseline"]
    with report_path.with_suffix(".log").open("w", encoding="utf-8") as log:
        result = subprocess.run(command, env=environment, stdout=log,
                                stderr=subprocess.STDOUT, timeout=300, check=False)
    print(report_path.with_suffix(".log").read_text(encoding="utf-8"), flush=True)
    require(result.returncode == 0, f"Rust assessment failed: exit={result.returncode}")
    report = json.loads(report_path.read_text(encoding="utf-8"))
    require(report["source"] == source, "Rust assessment source differs")
    require(report["attempt"] == attempt, "Rust assessment attempt differs")
    require(digest(binary) == binary_hash and digest(test_exe) == test_hash,
            "assessment executable changed during measurement")
    report.update(binary_sha256=binary_hash, test_sha256=test_hash,
                  runner_os=os.environ["RUNNER_OS"], runner_arch=os.environ["RUNNER_ARCH"],
                  image_os=os.environ["ImageOS"], image_version=os.environ["ImageVersion"],
                  git_version=subprocess.check_output(["git", "--version"], text=True).strip(),
                  workflow_run=os.environ["GITHUB_RUN_ID"],
                  workflow_attempt=os.environ["GITHUB_RUN_ATTEMPT"])
    report_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")


def baselines():
    rows = {}
    for line in (ROOT / "benchmarks" / "passthrough.csv").read_text().splitlines():
        if line.startswith(tuple(ARCHES)):
            name, baseline, limit = line.split(",")
            rows[name] = (float(baseline), float(limit))
    return rows


def aggregate(directory, expected_source=None):
    files = sorted(Path(directory).rglob("report.json"))
    require(len(files) == 9, f"expected all nine assessment reports, found {len(files)}")
    reports = [json.loads(path.read_text(encoding="utf-8")) for path in files]
    identities = set()
    observed = {}
    references = {}
    failures = []
    baseline_rows = baselines()
    for report in reports:
        operating_system, attempt = report["os"], report["attempt"]
        require(operating_system in ARCHES and type(attempt) is int and 1 <= attempt <= 3,
                "unexpected OS or assessment attempt")
        identity = operating_system, attempt
        require(identity not in identities, f"duplicate assessment {identity}")
        identities.add(identity)
        require(report["schema"] == 1 and report["arch"] == ARCHES[operating_system],
                f"unexpected schema or architecture for {identity}")
        require(report["samples_each"] == 201 and report["warmups_each"] == 16,
                f"measurement protocol changed for {identity}")
        if expected_source:
            require(report["source"] == expected_source, f"source differs for {identity}")
        for key in ("binary_sha256", "test_sha256"):
            value = report[key]
            require(len(value) == 64 and all(c in "0123456789abcdef" for c in value),
                    f"invalid executable hash for {identity}")
        # Patch image rollouts and Git updates are recorded per assessment.
        # Each paired measurement uses that host's native Git; only the OS
        # family/architecture and exact candidate/harness must stay fixed.
        provenance = tuple(report[key] for key in (
            "source", "binary_sha256", "test_sha256", "runner_os", "runner_arch",
            "image_os", "workflow_run", "workflow_attempt"))
        require(all(provenance), f"missing host provenance for {identity}")
        require(report["image_version"] and report["git_version"],
                f"missing image/Git observation for {identity}")
        if operating_system in references:
            require(provenance == references[operating_system],
                    f"source/binary/host-image/tool provenance differs for {identity}")
        references[operating_system] = provenance
        measurements = report["measurements"]
        require(len(measurements) == 2, f"missing or extra command for {identity}")
        commands = set()
        for measurement in measurements:
            command = tuple(measurement["args"])
            require(command in COMMANDS and command not in commands,
                    f"unexpected or duplicate command for {identity}")
            commands.add(command)
            baseline, limit = baseline_rows[operating_system]
            ceiling = round(min(limit, baseline * 1.5 + 1.0) * 1_000_000)
            require(measurement["baseline_ms"] == baseline and measurement["limit_ms"] == limit
                    and measurement["ceiling_ns"] == ceiling,
                    f"stored performance gate changed for {identity} {command}")
            samples = measurement["samples"]
            require(len(samples) == 201, f"missing samples for {identity} {command}")
            for sample in samples:
                require(all(type(sample[key]) is int for key in
                            ("git_ns", "paradox_ns", "difference_ns")), "invalid sample type")
                require(sample["git_ns"] >= 0 and sample["paradox_ns"] >= 0
                        and sample["difference_ns"] == sample["paradox_ns"] - sample["git_ns"],
                        "invalid signed paired sample")
            overhead = max(0, statistics.median(s["difference_ns"] for s in samples))
            require(measurement["overhead_ns"] == overhead
                    and measurement["git_median_ns"] == statistics.median(s["git_ns"] for s in samples)
                    and measurement["paradox_median_ns"] == statistics.median(s["paradox_ns"] for s in samples),
                    f"incorrect reported median for {identity} {command}")
            exceeded = overhead > ceiling
            require(type(measurement["exceeded"]) is bool and measurement["exceeded"] == exceeded,
                    f"incorrect reported vote for {identity} {command}")
            observed.setdefault((operating_system, command), []).append((attempt, overhead, exceeded))
    require(identities == {(name, attempt) for name in ARCHES for attempt in (1, 2, 3)},
            "incomplete assessment matrix")
    require(len({report["source"] for report in reports}) == 1, "cross-OS source differs")
    for (operating_system, command), votes in sorted(observed.items()):
        votes.sort()
        failed = sum(vote[2] for vote in votes) >= 2
        print(f"{operating_system} {list(command)}: fixed assessments={votes}, failed={failed}")
        if failed:
            failures.append((operating_system, command))
    require(not failures, f"persistent pass-through regressions: {failures}")
    return observed


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    pick = commands.add_parser("select")
    pick.add_argument("build_log")
    pick.add_argument("destination")
    run = commands.add_parser("record")
    run.add_argument("test_exe")
    run.add_argument("binary")
    run.add_argument("report")
    run.add_argument("attempt", type=int, choices=(1, 2, 3))
    vote = commands.add_parser("aggregate")
    vote.add_argument("directory")
    args = parser.parse_args()
    if args.command == "select":
        select(args.build_log, args.destination)
    elif args.command == "record":
        record(args.test_exe, args.binary, args.report, args.attempt)
    else:
        aggregate(args.directory, os.environ.get("GITHUB_SHA"))


if __name__ == "__main__":
    main()
