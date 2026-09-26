#!/usr/bin/env python3

import argparse
import math
import os
from pathlib import Path
import statistics
import subprocess
import tempfile
import time


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Build and benchmark AXE release optimization levels."
    )
    parser.add_argument(
        "levels",
        nargs="*",
        default=["z", "s", "2", "3"],
        choices=["z", "s", "2", "3"],
    )
    parser.add_argument("--runs", type=int, default=30)
    parser.add_argument("--warmups", type=int, default=5)
    return parser.parse_args()


def percentile(samples: list[float], fraction: float) -> float:
    return sorted(samples)[max(0, math.ceil(len(samples) * fraction) - 1)]


def measure(command: list[str], environment: dict[str, str], warmups: int, runs: int) -> tuple[float, float]:
    samples = []
    for index in range(warmups + runs):
        started = time.perf_counter_ns()
        result = subprocess.run(
            command,
            env=environment,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            check=False,
        )
        elapsed_ms = (time.perf_counter_ns() - started) / 1_000_000
        if result.returncode != 0:
            raise SystemExit(f"{' '.join(command)} exited with {result.returncode}")
        if index >= warmups:
            samples.append(elapsed_ms)

    return statistics.median(samples), percentile(samples, 0.95)


def build(root: Path, level: str) -> Path:
    target_dir = root / "target" / "opt-level-benchmark" / level
    environment = os.environ.copy()
    environment.update(
        {
            "CARGO_PROFILE_RELEASE_OPT_LEVEL": level,
            "CARGO_TARGET_DIR": str(target_dir),
            "CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER": str(root / "tools" / "rust-lld"),
        }
    )
    subprocess.run(
        [
            "cargo",
            "build",
            "--locked",
            "-p",
            "axe",
            "--release",
            "--target",
            "x86_64-unknown-linux-musl",
        ],
        cwd=root,
        env=environment,
        check=True,
    )
    return target_dir / "x86_64-unknown-linux-musl" / "release" / "axe"


def benchmark(root: Path, level: str, warmups: int, runs: int) -> dict[str, object]:
    binary = build(root, level)
    with tempfile.TemporaryDirectory(prefix=f"axe-opt-{level}-") as store:
        environment = {
            "HOME": "/tmp",
            "PATH": "/nonexistent",
            "AXE_STORE_DIR": store,
            "AXE_STORE_URL": "http://127.0.0.1:9",
            "AXE_STORE_ADDRESSES": "127.0.0.1",
        }
        workloads = {
            "version": [str(binary), "--version"],
            "bundled": [str(binary), "--invoke-bundled", "true"],
            "pipeline": [
                str(binary),
                "--no-config",
                "--norc",
                "--noprofile",
                "-c",
                "printf 'b\\na\\na\\n' | sort | uniq >/dev/null",
            ],
        }
        timings = {
            name: measure(command, environment, warmups, runs)
            for name, command in workloads.items()
        }

    return {"level": level, "bytes": binary.stat().st_size, "timings": timings}


def main() -> None:
    args = parse_args()
    if args.runs <= 0 or args.warmups < 0:
        raise SystemExit("--runs must be positive and --warmups must be nonnegative")

    root = Path(__file__).resolve().parent.parent
    subprocess.run(["just", "generate-dev-keys"], cwd=root, check=True)
    results = [benchmark(root, level, args.warmups, args.runs) for level in args.levels]

    print("| opt-level | size MiB | --version median/p95 ms | bundled median/p95 ms | pipeline median/p95 ms |")
    print("|---|---:|---:|---:|---:|")
    for result in results:
        timings = result["timings"]
        version = timings["version"]
        bundled = timings["bundled"]
        pipeline = timings["pipeline"]
        print(
            f"| {result['level']} | {result['bytes'] / (1024 * 1024):.2f} "
            f"| {version[0]:.2f}/{version[1]:.2f} "
            f"| {bundled[0]:.2f}/{bundled[1]:.2f} "
            f"| {pipeline[0]:.2f}/{pipeline[1]:.2f} |"
        )


if __name__ == "__main__":
    main()
