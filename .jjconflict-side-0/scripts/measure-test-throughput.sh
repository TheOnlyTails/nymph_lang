#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 1 || $# -gt 2 ]]; then
	echo "usage: $0 <build|1|2|4> [label]" >&2
	echo "       set NEXTEST_FILTER to measure a bounded nextest subset" >&2
	exit 2
fi

mode=$1
label=${2:-$mode}
sample_interval=${SAMPLE_INTERVAL:-0.2}
root=$(dirname "$(dirname "$(realpath "$0")")")
output_dir="$root/target/nextest/throughput-measurements"
mkdir -p "$output_dir"
stamp=$(date -u +%Y%m%dT%H%M%SZ)
prefix="$output_dir/$stamp-$label"
samples="$prefix-processes.tsv"
timing="$prefix-time.txt"
log="$prefix.log"

printf 'epoch_ms\tprocesses\trss_kb\tnode_processes\ttest_processes\n' >"$samples"

case "$mode" in
	build)
		command=(cargo test --workspace --all-features --no-run)
		;;
	1 | 2 | 4)
		command=(cargo nextest run --workspace --all-features --no-fail-fast --profile throughput -j "$mode")
		if [[ -n ${NEXTEST_FILTER:-} ]]; then
			command+=(-E "$NEXTEST_FILTER")
		fi
		;;
	*)
		echo "error: mode must be build, 1, 2, or 4" >&2
		exit 2
		;;
esac

printf 'command:' | tee "$log"
printf ' %q' "${command[@]}" | tee -a "$log"
printf '\n' | tee -a "$log"

/usr/bin/time -v -o "$timing" "${command[@]}" >>"$log" 2>&1 &
root_pid=$!

cleanup() {
	if ! kill -0 "$root_pid" 2>/dev/null; then
		return
	fi
	mapfile -t descendants < <(ps -eo pid=,ppid= | awk -v root="$root_pid" '
		{ pid[NR] = $1; ppid[NR] = $2 }
		END {
			selected[root] = 1
			for (pass = 0; pass < NR; pass++) {
				for (i = 1; i <= NR; i++) {
					if (selected[ppid[i]]) selected[pid[i]] = 1
				}
			}
			for (i = 1; i <= NR; i++) if (selected[pid[i]]) print pid[i]
		}
	')
	if ((${#descendants[@]} > 0)); then
		kill -TERM "${descendants[@]}" 2>/dev/null || true
	fi
}
trap cleanup EXIT
trap 'exit 130' HUP INT TERM

while [[ -n $(ps -o stat= -p "$root_pid" 2>/dev/null) ]] \
	&& [[ $(ps -o stat= -p "$root_pid" 2>/dev/null) != Z* ]]; do
	ps -eo pid=,ppid=,rss=,comm=,args= | awk -v root="$root_pid" -v now="$(date +%s%3N)" '
		{
			pid[NR] = $1; ppid[NR] = $2; rss[NR] = $3; comm[NR] = $4
			is_test[NR] = ($0 ~ /target\/debug\/deps\/.* --exact /)
		}
		END {
			selected[root] = 1
			for (pass = 0; pass < NR; pass++) {
				for (i = 1; i <= NR; i++) {
					if (selected[ppid[i]]) selected[pid[i]] = 1
				}
			}
			for (i = 1; i <= NR; i++) {
				if (!selected[pid[i]]) continue
				count++; total_rss += rss[i]
				if (comm[i] == "node") nodes++
				if (is_test[i]) tests++
			}
			printf "%s\t%d\t%d\t%d\t%d\n", now, count, total_rss, nodes, tests
		}
	' >>"$samples"
	sleep "$sample_interval"
done

set +e
wait "$root_pid"
status=$?
set -e
trap - EXIT HUP INT TERM

awk -F '\t' 'NR > 1 {
	if ($2 > max_processes) max_processes = $2
	if ($3 > max_rss) max_rss = $3
	if ($4 > max_nodes) max_nodes = $4
	if ($5 > max_tests) max_tests = $5
} END {
	printf "peak_processes=%d peak_rss_kb=%d peak_node_processes=%d peak_test_processes=%d\n",
		max_processes, max_rss, max_nodes, max_tests
}' "$samples" | tee -a "$log"
cat "$timing" | tee -a "$log"
printf 'artifacts=%s exit=%d\n' "$prefix" "$status" | tee -a "$log"
exit "$status"
