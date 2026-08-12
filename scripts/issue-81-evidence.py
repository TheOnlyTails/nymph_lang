#!/usr/bin/env python3
"""Reproduce, verify, and summarize issue #81's fresh-process evidence."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import statistics
import subprocess
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
DATA = ROOT / "docs/superpowers/benchmarks/issue-81-data"
BIN = ROOT / "target/release/issue_81_evidence"
SHAPES = ("single", "wide", "deep", "mixed")
WORKERS = (1, 2, 4, 8)
REQUESTS = ("diagnostics", "compile")


def run(command: list[str], *, env: dict[str, str] | None = None) -> subprocess.CompletedProcess[str]:
	return subprocess.run(command, cwd=ROOT, env=env, text=True, check=True, capture_output=True)


def build() -> None:
	subprocess.run(
		["cargo", "build", "--release", "-p", "nymph-compiler", "--features", "test-support", "--bin", "issue_81_evidence"],
		cwd=ROOT,
		check=True,
	)


def environment() -> dict[str, object]:
	def output(*command: str) -> str:
		return run(list(command)).stdout.strip()

	return {
		"platform": platform.platform(),
		"machine": platform.machine(),
		"python": platform.python_version(),
		"rustc": output("rustc", "--version", "--verbose"),
		"cargo": output("cargo", "--version"),
		"node": output("node", "--version"),
		"commit": output("git", "rev-parse", "HEAD"),
		"base_remote_tip": "269f87c963631f21ad53677c21e624e98f91f4f7",
		"controls": {"CARGO_INCREMENTAL": "0", "RAYON_NUM_THREADS": list(WORKERS)},
	}


def timed_sample(workers: int, shape: str, request: str, instrumentation: str) -> dict[str, object]:
	env = os.environ.copy()
	env["RAYON_NUM_THREADS"] = str(workers)
	with tempfile.NamedTemporaryFile() as timing:
		command = [
			"/usr/bin/time", "-o", timing.name, "-f", "%U %S %e %M",
			str(BIN), "sample", shape, request, instrumentation,
		]
		completed = run(command, env=env)
		timing.seek(0)
		user_s, system_s, wall_s, rss_kib = timing.read().decode().strip().split()
	row = json.loads(completed.stdout)
	row["process"] = {
		"user_s": float(user_s), "system_s": float(system_s),
		"wall_s": float(wall_s), "peak_rss_kib": int(rss_kib),
	}
	return row


def matrix(repeats: int) -> None:
	build()
	DATA.mkdir(parents=True, exist_ok=True)
	(DATA / "environment.json").write_text(json.dumps(environment(), indent=2, sort_keys=True) + "\n")
	commands = [
		"CARGO_INCREMENTAL=0 cargo build --release -p nymph-compiler --features test-support --bin issue_81_evidence",
		"RAYON_NUM_THREADS={1,2,4,8} /usr/bin/time -f '%U %S %e %M' target/release/issue_81_evidence sample {single,wide,deep,mixed} {diagnostics,compile} {uninstrumented,instrumented}",
		f"python3 scripts/issue-81-evidence.py matrix --repeats {repeats}",
	]
	(DATA / "commands.txt").write_text("\n".join(commands) + "\n")
	with (DATA / "raw.jsonl").open("w") as output:
		for workers in WORKERS:
			for shape in SHAPES:
				for request in REQUESTS:
					for repeat in range(1, repeats + 1):
						# Adjacent off/on processes form the instrumentation-overhead pair.
						for instrumentation in ("uninstrumented", "instrumented"):
							row = timed_sample(workers, shape, request, instrumentation)
							row["repeat"] = repeat
							profile = row.get("profile")
							if profile and profile["prewarm_max_active"] > profile["prewarm_configured_workers"]:
								raise AssertionError(f"prewarm bound exceeded: {row}")
							output.write(json.dumps(row, sort_keys=True, separators=(",", ":")) + "\n")
							output.flush()


def snapshots(repeats: int) -> None:
	build()
	DATA.mkdir(parents=True, exist_ok=True)
	baseline: dict[str, dict[str, object]] = {}
	with (DATA / "snapshots.jsonl").open("w") as output:
		for workers in WORKERS:
			for repeat in range(1, repeats + 1):
				env = os.environ.copy()
				env["RAYON_NUM_THREADS"] = str(workers)
				for shape in SHAPES:
					row = json.loads(run([str(BIN), "snapshot", shape], env=env).stdout)
					canonical = json.dumps(row, sort_keys=True, separators=(",", ":"))
					if shape in baseline and row != baseline[shape]:
						raise AssertionError(f"snapshot mismatch for {shape}, workers={workers}, repeat={repeat}")
					baseline.setdefault(shape, row)
					output.write(json.dumps({
						"workers": workers, "repeat": repeat, "snapshot": row,
						"sha256": hashlib.sha256(canonical.encode()).hexdigest(),
					}, sort_keys=True, separators=(",", ":")) + "\n")
	print(f"deterministic snapshots: {len(WORKERS) * repeats * len(SHAPES)} exact matches")


def median(values: list[float]) -> float:
	return statistics.median(values)


def summarize() -> None:
	rows = [json.loads(line) for line in (DATA / "raw.jsonl").read_text().splitlines()]
	groups: dict[tuple[int, str, str, bool], list[dict[str, object]]] = {}
	for row in rows:
		key = (row["rayon_workers"], row["shape"], row["request"], row["instrumented"])
		groups.setdefault(key, []).append(row)
	result: dict[str, object] = {
		"raw_rows": len(rows),
		"groups": {},
		"instrumentation_overhead_pct": {},
	}
	for key, items in sorted(groups.items()):
		workers, shape, request, instrumented = key
		cold = [item["cold_wall_ns"] / 1e6 for item in items]
		warm = [item["warm_ns_per_iteration"] / 1e3 for item in items]
		name = f"{workers}/{shape}/{request}/{'on' if instrumented else 'off'}"
		group = {
			"n": len(items), "cold_ms_mean": statistics.mean(cold), "cold_ms_median": median(cold),
			"cold_ms_min": min(cold), "cold_ms_max": max(cold), "warm_us_median": median(warm),
			"process_cpu_s_median": median([item["process"]["user_s"] + item["process"]["system_s"] for item in items]),
			"peak_rss_kib_max": max(item["process"]["peak_rss_kib"] for item in items),
		}
		if instrumented:
			group["prewarm_max_active_max"] = max(item["profile"]["prewarm_max_active"] for item in items)
			group["prewarm_configured_workers"] = items[0]["profile"]["prewarm_configured_workers"]
			group["phases_ms_median"] = {
				phase["name"]: median([
					next(p for p in item["profile"]["phases"] if p["name"] == phase["name"])["inclusive_ns"] / 1e6
					for item in items
				]) for phase in items[0]["profile"]["phases"]
			}
			group["phase_executions"] = {phase["name"]: phase["executions"] for phase in items[0]["profile"]["phases"]}
		result["groups"][name] = group
	all_overhead = []
	for workers in WORKERS:
		for shape in SHAPES:
			for request in REQUESTS:
				off = {row["repeat"]: row for row in groups[(workers, shape, request, False)]}
				on = {row["repeat"]: row for row in groups[(workers, shape, request, True)]}
				ratios = [(on[index]["cold_wall_ns"] / off[index]["cold_wall_ns"] - 1) * 100 for index in sorted(off)]
				all_overhead.extend(ratios)
				result["instrumentation_overhead_pct"][f"{workers}/{shape}/{request}"] = {
					"mean": statistics.mean(ratios), "median": median(ratios), "min": min(ratios), "max": max(ratios),
				}
	result["instrumentation_overhead_all_pairs_pct"] = {
		"n": len(all_overhead),
		"mean": statistics.mean(all_overhead),
		"median": median(all_overhead),
		"min": min(all_overhead),
		"max": max(all_overhead),
	}
	(DATA / "summary.json").write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
	print(json.dumps(result, indent=2, sort_keys=True))


def main() -> None:
	parser = argparse.ArgumentParser()
	sub = parser.add_subparsers(dest="command", required=True)
	for name, default in (("matrix", 5), ("snapshots", 3)):
		command = sub.add_parser(name)
		command.add_argument("--repeats", type=int, default=default)
	sub.add_parser("summarize")
	args = parser.parse_args()
	if args.command == "matrix":
		matrix(args.repeats)
	elif args.command == "snapshots":
		snapshots(args.repeats)
	else:
		summarize()


if __name__ == "__main__":
	main()
