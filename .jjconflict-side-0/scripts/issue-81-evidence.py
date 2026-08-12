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
RAW_REPEATS = 5
SNAPSHOT_REPEATS = 3
PHASES = (
	"parse", "interface_environment", "checker", "diagnostic_fold_wrapper",
	"stable_lowering", "emission", "bundling",
)
ARTIFACTS = ("raw.jsonl", "snapshots.jsonl", "summary.json", "environment.json", "commands.txt")


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


def summary(rows: list[dict[str, object]]) -> dict[str, object]:
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
	return result


def summarize() -> None:
	rows = [json.loads(line) for line in (DATA / "raw.jsonl").read_text().splitlines()]
	result = summary(rows)
	(DATA / "summary.json").write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
	print(json.dumps(result, indent=2, sort_keys=True))


def verify(data: Path) -> None:
	def jsonl(name: str) -> list[dict[str, object]]:
		path = data / name
		if not path.is_file():
			raise AssertionError(f"missing artifact: {path}")
		return [json.loads(line) for line in path.read_text().splitlines()]

	rows = jsonl("raw.jsonl")
	expected_cells = {
		(workers, shape, request, instrumented, repeat)
		for workers in WORKERS for shape in SHAPES for request in REQUESTS
		for instrumented in (False, True) for repeat in range(1, RAW_REPEATS + 1)
	}
	actual_cells = [
		(row.get("rayon_workers"), row.get("shape"), row.get("request"),
		 row.get("instrumented"), row.get("repeat")) for row in rows
	]
	if len(actual_cells) != len(expected_cells) or set(actual_cells) != expected_cells:
		raise AssertionError("raw matrix must contain each of the 320 expected cells exactly once")
	for row in rows:
		if row.get("kind") != "sample":
			raise AssertionError(f"invalid raw row kind: {row}")
		if row["warm_iterations"] < 10_000 or row["warm_total_ns"] < 200_000_000:
			raise AssertionError(f"warm-loop minimum not met: {row}")
		expected_warm = row["warm_total_ns"] / row["warm_iterations"]
		if abs(row["warm_ns_per_iteration"] - expected_warm) > expected_warm * 1e-12:
			raise AssertionError(f"warm-loop units mismatch: {row}")
		process = row.get("process", {})
		if process.get("user_s", -1) < 0 or process.get("system_s", -1) < 0 or process.get("peak_rss_kib", 0) <= 0:
			raise AssertionError(f"invalid process measurement: {row}")
		profile = row.get("profile")
		if not row["instrumented"]:
			if profile is not None:
				raise AssertionError(f"uninstrumented row contains a profile: {row}")
			continue
		if profile is None:
			raise AssertionError(f"instrumented row has no profile: {row}")
		if profile["prewarm_configured_workers"] != row["rayon_workers"]:
			raise AssertionError(f"configured worker mismatch: {row}")
		if not 0 < profile["prewarm_max_active"] <= profile["prewarm_configured_workers"]:
			raise AssertionError(f"prewarm bound exceeded: {row}")
		phases = profile.get("phases", [])
		if tuple(phase.get("name") for phase in phases) != PHASES:
			raise AssertionError(f"phase labels mismatch: {row}")
		modules = 1 if row["shape"] == "single" else 17
		expected_counts = (modules + 12, modules, modules, modules + 1) + (
			(0, 0, 0) if row["request"] == "diagnostics" else (modules, modules, 1)
		)
		if tuple(phase.get("executions") for phase in phases) != expected_counts:
			raise AssertionError(f"phase execution counts mismatch: {row}")

	snapshots = jsonl("snapshots.jsonl")
	expected_snapshots = {
		(workers, repeat, shape)
		for workers in WORKERS for repeat in range(1, SNAPSHOT_REPEATS + 1) for shape in SHAPES
	}
	actual_snapshots = [
		(item.get("workers"), item.get("repeat"), item.get("snapshot", {}).get("shape"))
		for item in snapshots
	]
	if len(actual_snapshots) != len(expected_snapshots) or set(actual_snapshots) != expected_snapshots:
		raise AssertionError("snapshot matrix must contain each of the 48 expected identities exactly once")
	baseline: dict[str, dict[str, object]] = {}
	for item in snapshots:
		payload = item["snapshot"]
		canonical = json.dumps(payload, sort_keys=True, separators=(",", ":"))
		if hashlib.sha256(canonical.encode()).hexdigest() != item.get("sha256"):
			raise AssertionError(f"snapshot hash mismatch: workers={item['workers']} repeat={item['repeat']}")
		shape = payload["shape"]
		if shape in baseline and payload != baseline[shape]:
			raise AssertionError(f"snapshot payload mismatch for {shape}")
		baseline.setdefault(shape, payload)

	retained_summary = json.loads((data / "summary.json").read_text())
	if retained_summary != summary(rows):
		raise AssertionError("summary.json does not derive exactly from raw.jsonl")
	environment = json.loads((data / "environment.json").read_text())
	if environment.get("controls") != {"CARGO_INCREMENTAL": "0", "RAYON_NUM_THREADS": list(WORKERS)}:
		raise AssertionError("environment controls mismatch")
	if not all(environment.get(name) for name in ("platform", "machine", "python", "rustc", "cargo", "node", "commit")):
		raise AssertionError("environment metadata is incomplete")
	commands = (data / "commands.txt").read_text().splitlines()
	if commands != [
		"CARGO_INCREMENTAL=0 python3 scripts/issue-81-evidence.py matrix --repeats 5",
		"python3 scripts/issue-81-evidence.py snapshots --repeats 3",
		"python3 scripts/issue-81-evidence.py summarize",
		"python3 scripts/issue-81-evidence.py verify",
	]:
		raise AssertionError("commands.txt does not contain the exact reproduction workflow")
	manifest = {}
	for line in (data / "artifacts.sha256").read_text().splitlines():
		digest, name = line.split("  ", 1)
		manifest[name] = digest
	for name in ARTIFACTS:
		digest = hashlib.sha256((data / name).read_bytes()).hexdigest()
		if manifest.get(name) != digest:
			raise AssertionError(f"artifact hash mismatch: {name}")
	print("verified 320 raw cells, 160 profile pairs, 48 snapshots, summary, metadata, and artifact hashes")


def main() -> None:
	parser = argparse.ArgumentParser()
	sub = parser.add_subparsers(dest="command", required=True)
	for name, default in (("matrix", 5), ("snapshots", 3)):
		command = sub.add_parser(name)
		command.add_argument("--repeats", type=int, default=default)
	sub.add_parser("summarize")
	verify_parser = sub.add_parser("verify")
	verify_parser.add_argument("--data-dir", type=Path, default=DATA)
	args = parser.parse_args()
	if args.command == "matrix":
		matrix(args.repeats)
	elif args.command == "snapshots":
		snapshots(args.repeats)
	elif args.command == "summarize":
		summarize()
	else:
		verify(args.data_dir)


if __name__ == "__main__":
	main()
