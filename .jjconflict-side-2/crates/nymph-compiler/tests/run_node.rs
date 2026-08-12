//! End-to-end: parse -> check -> lower -> emit -> run under Node, asserting stdout.

use std::io::Write;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

/// Compile a Nymph source module to a JS module string.
fn compile(src: &str) -> String {
	nymph_compiler::compile(src, "test")
		.unwrap_or_else(|diagnostics| panic!("compile errors: {diagnostics:?}"))
}

/// Append a driver that logs `call`, run the already-compiled `js` module under
/// Node, and return trimmed stdout.
fn run_js(mut js: String, call: &str) -> String {
	// Nymph values are boxed at the generated-JS boundary. Most tests in this
	// file assert a program's observable scalar result rather than its runtime
	// representation, so unwrap boxes before handing the value to `console.log`.
	// Tests that deliberately inspect representation still do so inside `call`
	// (for example with `JSON.stringify` or `[TAG]`) and therefore pass a raw JS
	// scalar/string through this helper unchanged.
	js.push_str(&format!(
		"\nfunction nymphTestValue(value) {{\n\
		\tif (typeof value === 'bigint') return Number(value);\n\
		\tif (value == null || typeof value !== 'object') return value;\n\
		\tif ('v' in value) {{\n\
		\t\treturn nymphTestValue(value.v);\n\
		\t}}\n\
		\tif (Array.isArray(value)) return value.map(nymphTestValue);\n\
		\tif (value instanceof Map) {{\n\
		\t\treturn [...value].map(([key, item]) => [nymphTestValue(key), nymphTestValue(item)]);\n\
		\t}}\n\
		\tif (typeof value[Symbol.iterator] === 'function') return [...value].map(nymphTestValue);\n\
		\tconst plain = {{}};\n\
		\tfor (const [key, item] of Object.entries(value)) plain[key] = nymphTestValue(item);\n\
		\treturn plain;\n\
		}}\n\
		console.log(nymphTestValue({call}));\n"
	));

	// `process::id()` alone is not a unique filename: all tests in this binary
	// share one process and may run on parallel threads, racing on the same path.
	// Mix in a monotonic counter to keep each test's script isolated.
	static COUNTER: AtomicU64 = AtomicU64::new(0);
	let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
	let dir = std::env::temp_dir();
	let path = dir.join(format!("nymph_run_{}_{unique}.mjs", std::process::id()));
	let mut file = std::fs::File::create(&path).unwrap();
	file.write_all(js.as_bytes()).unwrap();

	// This shell's environment may force ANSI color output (`FORCE_COLOR`), which
	// would corrupt the plain stdout values we assert on; pin Node to no-color.
	let output = Command::new("node")
		.arg(&path)
		.env("NO_COLOR", "1")
		.env_remove("FORCE_COLOR")
		.output()
		.expect("run node");
	let _ = std::fs::remove_file(&path);
	assert!(
		output.status.success(),
		"node failed:\n{}\n--- js ---\n{}",
		String::from_utf8_lossy(&output.stderr),
		js
	);
	String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// Emit `src`, append a driver that logs `expr`, run under Node, return trimmed stdout.
fn run(src: &str, call: &str) -> String {
	run_js(compile(src), call)
}

fn run_js_capturing_stderr(mut js: String, driver: &str) -> (String, String) {
	js.push_str(driver);
	static COUNTER: AtomicU64 = AtomicU64::new(0);
	let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
	let path = std::env::temp_dir().join(format!("nymph_echo_{}_{unique}.mjs", std::process::id()));
	std::fs::write(&path, js).unwrap();
	let output = Command::new("node").arg(&path).output().expect("run node");
	let _ = std::fs::remove_file(path);
	assert!(
		output.status.success(),
		"node failed: {}",
		String::from_utf8_lossy(&output.stderr)
	);
	(
		String::from_utf8_lossy(&output.stdout).trim().to_string(),
		String::from_utf8_lossy(&output.stderr).to_string(),
	)
}

#[test]
fn echo_returns_identity_renders_hidden_fields_and_keeps_opaque_values_inert() {
	let source = "public struct Secret(public shown: int, private hidden: int)\npublic func observed(value: int): int = echo value\npublic func secret(): Secret = echo Secret(shown = 1, hidden = 2)\npublic func once(make: () -> int): int = echo make()";
	let js = nymph_compiler::compile(source, "nested/main.nym")
		.unwrap_or_else(|diagnostics| panic!("compile errors: {diagnostics:?}"));
	let (stdout, stderr) = run_js_capturing_stderr(
		js,
		"\nconst value = new NInt(7);\n\
		 let same = observed(value) === value;\n\
		 const structural = secret();\n\
		 let callbacks = 0;\n\
		 structural.debug = () => { callbacks++; throw new Error('debug'); };\n\
		 structural.toString = () => { callbacks++; throw new Error('toString'); };\n\
		 nymphEcho(structural, { file: '<struct>', line: 1, column: 1, uri: null });\n\
		 const opaque = new Proxy({}, { get() { callbacks++; throw new Error('getter'); }, ownKeys() { callbacks++; throw new Error('keys'); }, getOwnPropertyDescriptor() { callbacks++; throw new Error('descriptor'); } });\n\
		 const opaqueSame = nymphEcho(opaque, { file: '<expr>', line: 1, column: 1, uri: null }) === opaque;\n\
		 let evaluations = 0;\n\
		 const made = new NInt(8);\n\
		 const onceSame = once(() => { evaluations++; return made; }) === made;\n\
		 console.log(`${same} ${opaqueSame} ${onceSame} ${evaluations} ${callbacks}`);\n",
	);
	assert_eq!(stdout, "true true true 1 0");
	let lines = stderr.lines().collect::<Vec<_>>();
	assert_eq!(lines.len(), 5, "{stderr}");
	assert!(lines[0].starts_with("main.nym:2:41: 7"), "{stderr}");
	assert!(lines[1].contains("Secret(shown: 1, hidden: 2)"), "{stderr}");
	assert!(lines[2].contains("Secret(shown: 1, hidden: 2)"), "{stderr}");
	assert_eq!(lines[3], "<expr>:1:1: <opaque external>");
	assert!(lines[4].ends_with(": 8"), "{stderr}");
}

#[test]
fn echo_is_atomic_interactive_and_cannot_change_control_flow() {
	let js = compile("public func observed(value: int): int = echo value");
	let (stdout, stderr) = run_js_capturing_stderr(
		js,
		r#"
const originalWrite = process.stderr.write;
const ttyDescriptor = Object.getOwnPropertyDescriptor(process.stderr, "isTTY");
const writes = [];
Object.defineProperty(process.stderr, "isTTY", { value: true, configurable: true });
process.stderr.write = (line) => { writes.push(line); return true; };
let callbacks = 0;
const opaque = new Proxy({}, {
	get() { callbacks++; throw new Error("getter"); },
	ownKeys() { callbacks++; throw new Error("keys"); },
	getOwnPropertyDescriptor() { callbacks++; throw new Error("descriptor"); },
});
const callable = () => {};
callable.toString = () => { callbacks++; return "called"; };
const value = new NInt(9n);
const same = nymphEcho(value, { file: "main.nym", line: 4, column: 7, uri: "file:///tmp/main.nym" }) === value;
nymphEcho(opaque, { file: "main.nym", line: 5, column: 7, uri: null });
nymphEcho(callable, { file: "main.nym", line: 6, column: 7, uri: null });
process.stderr.write = () => { throw new Error("write failed"); };
const survives = nymphEcho(value, { file: "main.nym", line: 7, column: 7, uri: null }) === value;
process.stderr.write = originalWrite;
if (ttyDescriptor) Object.defineProperty(process.stderr, "isTTY", ttyDescriptor);
else delete process.stderr.isTTY;
console.log(JSON.stringify({ same, survives, callbacks, writes }));
"#,
	);
	assert!(stderr.is_empty(), "{stderr}");
	let result: serde_json::Value = serde_json::from_str(&stdout).unwrap();
	assert_eq!(result["same"], true);
	assert_eq!(result["survives"], true);
	assert_eq!(result["callbacks"], 0);
	let writes = result["writes"].as_array().unwrap();
	assert_eq!(writes.len(), 3, "{writes:?}");
	assert!(
		writes[0]
			.as_str()
			.unwrap()
			.contains("\u{1b}]8;;file:///tmp/main.nym#L4:7")
	);
	assert!(
		writes
			.iter()
			.all(|write| write.as_str().unwrap().ends_with('\n'))
	);
	assert!(writes[1].as_str().unwrap().ends_with("<opaque external>\n"));
	assert!(writes[2].as_str().unwrap().ends_with("<function>\n"));
}

#[test]
fn inferred_explicit_closure_return_exits_the_closure_not_its_creator() {
	let src = "func choose(flag: boolean): int = {\n\tlet pick = (flag: boolean) -> { if (flag) { return 7 } 9 }\n\tlet value = pick(flag)\n\treturn value + 1\n}";
	assert_eq!(run(src, "choose(new NBool(true))"), "8");
	assert_eq!(run(src, "choose(new NBool(false))"), "10");
}

fn run_failure(src: &str, call: &str) -> String {
	let mut js = compile(src);
	js.push_str(&format!("\n{call};\n"));
	static COUNTER: AtomicU64 = AtomicU64::new(0);
	let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
	let path =
		std::env::temp_dir().join(format!("nymph_failure_{}_{unique}.mjs", std::process::id()));
	std::fs::write(&path, js).unwrap();
	let output = Command::new("node").arg(&path).output().expect("run node");
	let _ = std::fs::remove_file(path);
	assert!(!output.status.success(), "node unexpectedly succeeded");
	String::from_utf8_lossy(&output.stderr).to_string()
}

#[test]
fn runs_arithmetic() {
	// Pure scalar arithmetic (Task 3/4 already cover emit+lower; this asserts it RUNS).
	let out = run(
		"func add(a: int, b: int): int = a + b * 2",
		"add(new NInt(3), new NInt(4))",
	);
	assert_eq!(out, "11");
}

#[test]
fn runs_an_operator_inside_a_string_interpolation() {
	// Regression: an interpolated expression used to be parsed by a FRESH sub-parser
	// whose node ids restarted at 0, colliding with the surrounding tree's — so the
	// operator's recorded dispatch was clobbered by whatever unrelated node shared its
	// id, surfacing at lowering as "no operator resolution recorded". The interpolated
	// `${a + b}` (and a call/closure inside one) must run.
	let out = run(
		"func f(a: int, b: int): string = \"sum=${a + b}\"",
		"f(new NInt(3), new NInt(4))",
	);
	assert_eq!(out, "sum=7");
	let out2 = run(
		"func apply(g: (int, int) -> int, a: int, b: int): int = g(a, b)\n\
		 func f(): string = \"r=${apply((x: int, y: int) -> x + y, 3, 4)}\"",
		"f()",
	);
	assert_eq!(out2, "r=7");
}

#[test]
fn runs_a_block_with_bindings() {
	let src = r#"
		func compute(): int = {
			let x = 10
			let y = x + 5
			y * 2
		}
	"#;
	let out = run(src, "compute()");
	assert_eq!(out, "30");
}

#[test]
fn runs_if_as_value() {
	// `if`/`else` in value position (nested), each branch a block with a tail value.
	let src = r#"
		func sign(n: int): int =
			if (n > 0) { 1 }
			else { if (n < 0) { -1 } else { 0 } }
	"#;
	assert_eq!(run(src, "sign(new NInt(5))"), "1");
	assert_eq!(run(src, "sign(new NInt(-3))"), "-1");
	assert_eq!(run(src, "sign(new NInt(0))"), "0");
}

#[test]
fn runs_list_and_index() {
	let src = r#"
		func at(i: int): int = #[10, 20, 30][i]
		func at_unsigned(i: uint): int = #[10, 20, 30][i]
	"#;
	assert_eq!(run(src, "at(new NInt(-1n))"), "30");
	assert_eq!(run(src, "at_unsigned(new NUint(1n))"), "20");
}

#[test]
fn runs_custom_index_impl() {
	let src = r#"
		interface Index<Key, Output> { func index(key: Key): Output }
		struct Offset(base: int) {
			impl Index<Key = int, Output = int> {
				func index(key: int): int = this.base + key
			}
		}
		func lookup(): int = Offset(base = 40)[2]
	"#;
	assert_eq!(run(src, "lookup().v"), "42");
}

#[test]
fn runs_tuple_roundtrip() {
	// A tuple emits as a JS array — `JSON.stringify` proves the shape survives.
	let src = "func pair(): #(int, int) = #(1, 2)";
	assert_eq!(run(src, "JSON.stringify(nymphTestValue(pair()))"), "[1,2]");
}

#[test]
fn runs_map_get() {
	// A map emits as `new Map([[k, v], …])`; indexing dispatches to `.get(key)`.
	// Int keys keep this slice free of string-literal lowering (a later slice).
	let src = "func lookup(): int = #{ 1: 5, 2: 6 }[2]";
	assert_eq!(run(src, "lookup()"), "6");
}

#[test]
fn runs_struct_construction_and_field() {
	// A struct constructs as `new Class({…})`; a field reads back as `.field`.
	let src = r#"
		struct Point(x: int, y: int)
		func make(): Point = Point(x = 3, y = 4)
	"#;
	assert_eq!(run(src, "make().y"), "4");
}

#[test]
fn runs_struct_field_through_param() {
	// A struct passed as a parameter; fields summed. Proves the class ctor matches
	// the object shape the JS driver constructs.
	let src = r#"
		struct Point(x: int, y: int)
		func sum(p: Point): int = p.x + p.y
	"#;
	assert_eq!(
		run(src, "sum(new Point({ x: new NInt(10), y: new NInt(20) }))"),
		"30"
	);
}

#[test]
fn runs_enum_field_variant() {
	// A field variant constructs via its factory; a field reads back.
	let src = r#"
		enum Opt { Some(value: int), None }
		func mk(): Opt = Some(value = 7)
	"#;
	assert_eq!(run(src, "mk().value"), "7");
}

#[test]
fn runs_enum_nullary_identity() {
	// A nullary variant is a frozen singleton: every reference is identical.
	let src = r#"
		enum Opt { Some(value: int), None }
		func none(): Opt = None
	"#;
	assert_eq!(run(src, "none() === Opt.None"), "true");
}

#[test]
fn runs_enum_variant_tag_distinct() {
	// Variants carry the shared TAG symbol; distinct variants have distinct tags.
	let src = r#"
		enum A { X(n: int), Y }
	"#;
	let tag = "Symbol.for('nymph.tag')";
	// A constructed X shares X's tag (the factory takes an object arg), and X's tag
	// differs from Y's.
	assert_eq!(
		run(src, &format!("A.X({{ n: 1 }})[{tag}] === A.X[{tag}]")),
		"true"
	);
	assert_eq!(run(src, &format!("A.X[{tag}] === A.Y[{tag}]")), "false");
}

#[test]
fn runs_match_variant_binding() {
	// `match` over an enum, binding a field variant's payload; nullary falls through.
	let src = r#"
		enum Opt { Some(value: int), None }
		func unwrap_or(o: Opt): int = match (o) {
			Some(value) -> value,
			None -> 0,
		}
	"#;
	assert_eq!(run(src, "unwrap_or(Opt.Some({ value: 42 }))"), "42");
	assert_eq!(run(src, "unwrap_or(Opt.None)"), "0");
}

#[test]
fn runs_match_literal_and_wildcard() {
	// Scalar literal arms plus a wildcard fallback.
	let src = r#"
		func classify(n: int): int = match (n) {
			0 -> 100,
			1 -> 200,
			_ -> 300,
		}
	"#;
	assert_eq!(run(src, "classify(new NInt(0))"), "100");
	assert_eq!(run(src, "classify(new NInt(1))"), "200");
	assert_eq!(run(src, "classify(new NInt(9))"), "300");
}

#[test]
fn runs_match_nested_variant() {
	// A variant pattern nested inside another (`Wrap(i = A(n))`): the field subject
	// path `_s.i` is itself matched against a variant, binding `n` from `_s.i.n`.
	let src = r#"
		enum Inner { A(n: int), B }
		enum Outer { Wrap(i: Inner), Nil }
		func f(o: Outer): int = match (o) {
			Wrap(i = A(n)) -> n,
			Wrap(i = B) -> 0,
			Nil -> -1,
		}
	"#;
	assert_eq!(run(src, "f(Outer.Wrap({ i: Inner.A({ n: 5 }) }))"), "5");
	assert_eq!(run(src, "f(Outer.Wrap({ i: Inner.B }))"), "0");
	assert_eq!(run(src, "f(Outer.Nil)"), "-1");
}

#[test]
fn runs_match_tuple_and_guard() {
	// Tuple destructuring by index, plus a guard that falls through when it fails.
	let src = r#"
		func f(p: #(int, int)): int = match (p) {
			#(0, y) -> y,
			#(x, _) if x > 10 -> x,
			#(x, _) -> 0,
		}
	"#;
	assert_eq!(run(src, "f(new NTuple([new NInt(0), new NInt(7)]))"), "7"); // first arm (literal 0 matches)
	assert_eq!(run(src, "f(new NTuple([new NInt(20), new NInt(1)]))"), "20"); // guard passes
	assert_eq!(run(src, "f(new NTuple([new NInt(5), new NInt(1)]))"), "0"); // guard fails → fall through
}

#[test]
fn runs_match_struct_pattern() {
	// A struct pattern is irrefutable: it just binds fields.
	let src = r#"
		struct Point(x: int, y: int)
		func f(pt: Point): int = match (pt) {
			Point(x = px, y = py) -> px + py,
		}
	"#;
	assert_eq!(
		run(src, "f(new Point({ x: new NInt(3), y: new NInt(4) }))"),
		"7"
	);
}

#[test]
fn runs_immutable_struct_defaults_and_clone_updates_in_exact_order() {
	let src = r#"
		func mark(value: int): int = value
		struct Record(a: int = mark(1), b: int = mark(2), c: int)
		func mark_source(value: Record): Record = value
		func fresh(): Record = Record(c = mark(3))
		func clone(source: Record): Record = Record(...mark_source(source), b = mark(4), c = mark(6))
		func unchanged(): int = { let source = Record(a = 1, b = 2, c = 3)
			let updated = Record(...source, b = 4)
			source.b * 10 + updated.b }
	"#;
	assert_eq!(
		run(
			src,
			"(() => { let seen = 0; mark = value => { seen = seen * 10 + Number(value.v); return value }; fresh(); return seen })()",
		),
		"312"
	);
	assert_eq!(
		run(
			src,
			"(() => { let seen = 0; mark = value => { seen = seen * 10 + Number(value.v); return value }; mark_source = value => { seen = seen * 10 + 5; return value }; clone(new Record({ a: new NInt(1), b: new NInt(2), c: new NInt(3) })); return seen })()",
		),
		"546"
	);
	assert_eq!(run(src, "unchanged()"), "24");
}

#[test]
fn runs_match_as_subexpression() {
	// `match` in value position (an operand of `+`) collapses to an IIFE.
	let src = r#"
		enum Opt { Some(value: int), None }
		func f(o: Opt): int = 1 + match (o) {
			Some(value) -> value,
			None -> 0,
		}
	"#;
	assert_eq!(run(src, "f(Opt.Some({ value: new NInt(41) }))"), "42");
	assert_eq!(run(src, "f(Opt.None)"), "1");
}

#[test]
fn runs_match_list_patterns() {
	// Exact-length (`#[]`) and spread (`#[a, ...rest]`) list patterns.
	// The checker requires a `_` arm for list matches (it does not infer that empty +
	// spread covers all lists); the wildcard is unreachable at runtime here.
	let src = r#"
		func head_or(xs: #[int]): int = match (xs) {
			#[] -> -1,
			#[a, ...rest] -> a,
			_ -> 0,
		}
	"#;
	assert_eq!(run(src, "head_or(new NList([]))"), "-1"); // exact-length #[] arm
	assert_eq!(
		run(
			src,
			"head_or(new NList([new NInt(7), new NInt(8), new NInt(9)]))"
		),
		"7"
	); // spread arm binds head
}

#[test]
fn runs_match_list_rest_with_suffix() {
	// `#[a, ...mid, b]` — a bound head, a rest slice, and a bound tail-from-the-end.
	let src = r#"
		func ends(xs: #[int]): int = match (xs) {
			#[a, ...mid, b] -> a + b,
			_ -> -1,
		}
	"#;
	assert_eq!(
		run(
			src,
			"ends(new NList([new NInt(10), new NInt(2), new NInt(3), new NInt(20)]))"
		),
		"30"
	); // a=10, b=20 (mid=[2,3])
	assert_eq!(
		run(src, "ends(new NList([new NInt(1), new NInt(9)]))"),
		"10"
	); // a=1, b=9, mid=[]
	assert_eq!(run(src, "ends(new NList([new NInt(5)]))"), "-1"); // length 1 < 2 → wildcard
}

#[test]
fn runs_match_tuple_rest_with_suffix() {
	// `#(a, ...mid, z)` — a tuple rest pattern in `match`. A tuple is irrefutable
	// (one static shape), so no `_` fallback arm is required.
	let src = r#"
		func ends(t: #(int, boolean, char, int)): int = match (t) {
			#(a, ...mid, z) -> a + z,
		}
	"#;
	assert_eq!(
		run(
			src,
			"ends(new NTuple([new NInt(10), new NBool(true), new NString('y'), new NInt(20)]))"
		),
		"30"
	);
}

#[test]
fn runs_tuple_rest_binds_middle_subtuple() {
	// `#(a, ...rest, z)` — `rest` binds the heterogeneous middle sub-tuple
	// (boolean, char), sliced from the tuple's own concrete element types.
	// (Destructuring `let` patterns are a separate, pre-existing lowering
	// limitation — "slice-1 lowering supports only identifier params" — unrelated
	// to pattern rest, so this drives the binding through `match` instead.)
	let src = r#"
		func mid(t: #(int, boolean, char, int)): #(boolean, char) = match (t) {
			#(a, ...rest, z) -> rest,
		}
	"#;
	assert_eq!(
		run(
			src,
			"JSON.stringify(nymphTestValue(mid(new NTuple([new NInt(1), new NBool(true), new NString('x'), new NInt(4)]))))"
		),
		r#"[true,"x"]"#
	);
}

#[test]
fn runs_match_map_pattern() {
	// A map pattern tests `.has(key)` and binds `.get(key)`.
	let src = r#"
		func lookup(m: #{int: int}): int = match (m) {
			#{ 1: v } -> v,
			_ -> -1,
		}
	"#;
	assert_eq!(
		run(src, "lookup(new NMap([[new NInt(1), new NInt(42)]]))"),
		"42"
	);
	assert_eq!(
		run(src, "lookup(new NMap([[new NInt(2), new NInt(9)]]))"),
		"-1"
	);
}

#[test]
fn runs_match_map_pattern_rest() {
	// `#{ 1: v, ...rest }` — a named-key bind plus a rest-of-map bind (a shallow
	// copy of the scrutinee minus the named keys).
	let src = r#"
		func without_one(m: #{int: int}): #{int: int} = match (m) {
			#{ 1: v, ...rest } -> rest,
			_ -> m,
		}
	"#;
	assert_eq!(
		run(
			src,
			"JSON.stringify(nymphTestValue(without_one(new NMap([[new NInt(1), new NInt(10)], [new NInt(2), new NInt(20)], [new NInt(3), new NInt(30)]]))).sort(([a], [b]) => a - b))"
		),
		r#"[[2,20],[3,30]]"#
	);
	// The `1` key is absent, so the wildcard arm returns `m` unchanged.
	assert_eq!(
		run(
			src,
			"JSON.stringify(nymphTestValue(without_one(new NMap([[new NInt(2), new NInt(20)]]))).sort(([a], [b]) => a - b))"
		),
		r#"[[2,20]]"#
	);
}

#[test]
fn runs_match_map_pattern_rest_with_no_named_keys() {
	// `#{ ...rest }` with no named entries reuses the immutable map.
	let src = r#"
		func copy(m: #{int: int}): #{int: int} = match (m) {
			#{ ...rest } -> rest,
			_ -> m,
		}
	"#;
	assert_eq!(
		run(
			src,
			"JSON.stringify(nymphTestValue(copy(new NMap([[new NInt(1), new NInt(1)]]))))",
		),
		"[[1,1]]"
	);
	let persistence_check = r#"
		const original = new NMap([[new NInt(1), new NInt(1)]]);
		const result = copy(original);
		const extended = result.with(new NInt(2), new NInt(2));
		return JSON.stringify(nymphTestValue([original, extended]));
	"#;
	assert_eq!(
		run(src, &format!("(() => {{ {persistence_check} }})()")),
		r#"[[[1,1]],[[1,1],[2,2]]]"#
	);
}

#[test]
fn runs_match_range_and_string() {
	let n = r#"
		func size(n: int): int = match (n) {
			1..10 -> 1,
			10..=100 -> 2,
			_ -> 3,
		}
	"#;
	assert_eq!(run(n, "size(new NInt(5))"), "1");
	assert_eq!(run(n, "size(new NInt(100))"), "2");
	assert_eq!(run(n, "size(new NInt(500))"), "3");
}

#[test]
fn runs_match_union() {
	// A union of nullary variants tests either tag.
	let src = r#"
		enum Color { Red, Green, Blue }
		func warm(c: Color): boolean = match (c) {
			Red | Green -> true,
			Blue -> false,
		}
	"#;
	assert_eq!(run(src, "warm(Color.Red)"), "true");
	assert_eq!(run(src, "warm(Color.Green)"), "true");
	assert_eq!(run(src, "warm(Color.Blue)"), "false");
}

#[test]
fn bound_union_selects_each_binding_from_the_matching_alternative() {
	let src = r#"
		func select(value: #(int, int)): #(int, int) = match (value) {
			#(x = 1, y = 2) | #(y = 3, x = 4) -> #(x, y),
			_ -> #(0, 0),
		}
	"#;
	assert_eq!(
		run(src, "select(new NTuple([new NInt(1), new NInt(2)]))"),
		"[ 1, 2 ]"
	);
	assert_eq!(
		run(src, "select(new NTuple([new NInt(3), new NInt(4)]))"),
		"[ 4, 3 ]"
	);
}

#[test]
fn bound_union_tests_once_and_uses_leftmost_matching_extraction() {
	let src = r#"
		func select(value: #(int, int)): #(int, int) = match (value) {
			#(x = 1, y = 2) | #(y, x) -> #(x, y),
		}
	"#;
	let call = r#"(() => {
		let reads = 0;
		const values = new Proxy([new NInt(1), new NInt(2)], {
			get(target, key) {
				if (key === "0" || key === "1") reads += 1;
				return target[key];
			}
		});
		const selected = select(new NTuple(values));
		return new NTuple([selected, new NInt(reads)]);
	})()"#;
	assert_eq!(run(src, call), "[ [ 1, 2 ], 4 ]");
}

#[test]
fn bound_union_source_names_cannot_collide_with_compiler_temporaries() {
	let src = r#"
		func select(value: #(int, int)): #(int, int) = match (value) {
			#(_t0 = 1, _t3 = 2) | #(_t3 = 3, _t0 = 4) -> #(_t0, _t3),
			_ -> #(0, 0),
		}
	"#;
	assert_eq!(
		run(src, "select(new NTuple([new NInt(1), new NInt(2)]))"),
		"[ 1, 2 ]"
	);
	assert_eq!(
		run(src, "select(new NTuple([new NInt(3), new NInt(4)]))"),
		"[ 4, 3 ]"
	);
}

#[test]
fn nested_bound_unions_select_all_branches_in_source_order() {
	let src = r#"
		func left_nested(value: int): int = match (value) {
			((x = 1 | x = 2) | x = 3) -> x,
			_ -> 0,
		}
		func right_nested(value: int): int = match (value) {
			(x = 1 | (x = 2 | x = 3)) -> x,
			_ -> 0,
		}
	"#;
	for name in ["left_nested", "right_nested"] {
		for value in 1..=3 {
			assert_eq!(
				run(src, &format!("{name}(new NInt({value}))")),
				value.to_string()
			);
		}
	}
}

#[test]
fn nested_destructuring_union_extracts_only_the_matching_plan() {
	let src = r#"
		func select(value: #(int, int)): #(int, int) = match (value) {
			#((x = 1 | x = 2), y) | #(y = 3, x) -> #(x, y),
			_ -> #(0, 0),
		}
	"#;
	assert_eq!(
		run(src, "select(new NTuple([new NInt(2), new NInt(8)]))"),
		"[ 2, 8 ]"
	);
	assert_eq!(
		run(src, "select(new NTuple([new NInt(3), new NInt(9)]))"),
		"[ 9, 3 ]"
	);
}

#[test]
fn runs_struct_method_with_this() {
	// An inherent method emits as a class method; `this` reads the instance's fields.
	let src = r#"
		struct Point(x: int, y: int)
		impl Point {
			func sum(): int = this.x + this.y
		}
		func total(p: Point): int = p.sum()
	"#;
	assert_eq!(
		run(src, "total(new Point({ x: new NInt(3), y: new NInt(4) }))"),
		"7"
	);
}

#[test]
fn runs_struct_method_with_args() {
	// A method taking a parameter, called positionally.
	let src = r#"
		struct Counter(n: int)
		impl Counter {
			func add(k: int): int = this.n + k
		}
		func bump(c: Counter): int = c.add(10)
	"#;
	assert_eq!(run(src, "bump(new Counter({ n: new NInt(5) }))"), "15");
}

#[test]
fn runs_struct_method_with_if_control_flow() {
	// A method body uses `if`/`else` as a value, branching on `this` field access.
	let src = r#"
		struct Point(x: int, y: int)
		impl Point {
			func biggest(): int = if (this.x > this.y) this.x else this.y
		}
		func main(): int = Point(x = 3, y = 9).biggest()
	"#;
	assert_eq!(run(src, "main()"), "9");
}

#[test]
fn runs_struct_method_calls_sibling_method() {
	// A method calls another method on the same struct via `this`.
	let src = r#"
		struct Counter(n: int)
		impl Counter {
			func base(): int = this.n
			func doubled(): int = this.base() + this.base()
		}
	"#;
	assert_eq!(run(src, "new Counter({ n: new NInt(21) }).doubled()"), "42");
}

#[test]
fn runs_struct_inner_func() {
	// A method declared inside the struct body itself (not a top-level `impl` block).
	let src = r#"
		struct Point(x: int, y: int) {
			func sum(): int = this.x + this.y
		}
	"#;
	assert_eq!(
		run(src, "new Point({ x: new NInt(10), y: new NInt(5) }).sum()"),
		"15"
	);
}

#[test]
fn runs_operator_overload_via_nested_impl() {
	// `+` on a struct with a NESTED `impl Plus<...>` (declared inside the struct
	// body) dispatches to `.plus(...)` rather than a native JS `+` (Slice 4B, D3/D4).
	let src = r#"
		interface Plus<Other, Output> { func plus(other: Other): Output }
		struct Vec2(x: int, y: int) {
			impl Plus<Other = Vec2, Output = Vec2> {
				func plus(other: Vec2): Vec2 = Vec2(x = this.x + other.x, y = this.y + other.y)
			}
		}
		func add(a: Vec2, b: Vec2): Vec2 = a + b
	"#;
	assert_eq!(
		run(
			src,
			"add(new Vec2({ x: new NInt(1), y: new NInt(2) }), new Vec2({ x: new NInt(3), y: new NInt(4) })).x"
		),
		"4"
	);
}

#[test]
fn runs_operator_overload_via_top_level_impl() {
	// Same behavior as the nested case, but the impl is a TOP-LEVEL
	// `impl Plus<...> for Vec2` block rather than nested inside the struct body.
	let src = r#"
		interface Plus<Other, Output> { func plus(other: Other): Output }
		struct Vec2(x: int, y: int)
		impl Plus<Other = Vec2, Output = Vec2> for Vec2 {
			func plus(other: Vec2): Vec2 = Vec2(x = this.x + other.x, y = this.y + other.y)
		}
		func add(a: Vec2, b: Vec2): Vec2 = a + b
	"#;
	assert_eq!(
		run(
			src,
			"add(new Vec2({ x: new NInt(1), y: new NInt(2) }), new Vec2({ x: new NInt(3), y: new NInt(4) })).x"
		),
		"4"
	);
}

#[test]
fn runs_operator_inside_method_body_stays_native_but_outer_dispatches() {
	// The outer `a + b` inside `combine` dispatches to `.plus(...)` (a `UserImpl`
	// resolution); the inner `this.x + other.x` inside the method body itself is
	// `float + float`, which stays a native JS `+` (`BuiltinEager`).
	let src = r#"
		interface Plus<Other, Output> { func plus(other: Other): Output }
		struct Vec2(x: float, y: float)
		impl Plus<Other = Vec2, Output = float> for Vec2 {
			func plus(other: Vec2): float = this.x + other.x
		}
		func combine(a: Vec2, b: Vec2): float = a + b
	"#;
	assert_eq!(
		run(
			src,
			"combine(new Vec2({ x: new NInt(1), y: new NInt(2) }), new Vec2({ x: new NInt(3), y: new NInt(4) }))"
		),
		"4"
	);
}

#[test]
fn runs_mixed_int_and_float_stays_native() {
	// An `int` literal against a `float` operand widens rather than dispatching to
	// an overload (no impl needed) — this stays a native JS `+`.
	let src = "func bump(x: float): float = x + 1";
	assert_eq!(run(src, "bump(new NFloat(2.5))"), "3.5");
}

#[test]
fn runs_prefix_negate_overload_dispatches_to_method() {
	// `-v` on a struct with a directly-defined `Negate.negate` impl actually calls
	// `.negate()` at runtime, componentwise negating the vector (Slice 4C-a).
	let src = r#"
		interface Negate<Output> { func negate(): Output }
		struct Vec2(x: int, y: int)
		impl Negate<Output = Vec2> for Vec2 {
			func negate(): Vec2 = Vec2(x = -this.x, y = -this.y)
		}
		func flip(v: Vec2): Vec2 = -v
	"#;
	assert_eq!(
		run(src, "flip(new Vec2({ x: new NInt(1), y: new NInt(2) })).x"),
		"-1"
	);
	assert_eq!(
		run(src, "flip(new Vec2({ x: new NInt(1), y: new NInt(2) })).y"),
		"-2"
	);
}

#[test]
fn runs_prefix_bool_not_and_native_int_float_negate_stay_native() {
	// `!boolean` and `-int`/`-float` stay native JS unary operators — no impl in
	// scope, `BuiltinEager` resolution.
	assert_eq!(
		run("func f(b: boolean): boolean = !b", "f(new NBool(true))"),
		"false"
	);
	assert_eq!(run("func f(x: int): int = -x", "f(new NInt(5))"), "-5");
	assert_eq!(
		run("func f(x: float): float = -x", "f(new NFloat(2.5))"),
		"-2.5"
	);
}

#[test]
fn runs_prefix_operator_inside_method_body_stays_native_but_outer_dispatches() {
	// The outer `-v` dispatches to `.negate()` (a `UserImpl` resolution); the inner
	// `this.x * -1` inside the method body itself is `int * -1` — the `-1` there is
	// a native unary negate on a literal, not a dispatched call.
	let src = r#"
		interface Negate<Output> { func negate(): Output }
		struct Vec2(x: int, y: int)
		impl Negate<Output = Vec2> for Vec2 {
			func negate(): Vec2 = Vec2(x = this.x * -1, y = this.y * -1)
		}
		func flip(v: Vec2): Vec2 = -v
	"#;
	assert_eq!(
		run(src, "flip(new Vec2({ x: new NInt(1), y: new NInt(2) })).x"),
		"-1"
	);
}

#[test]
fn runs_prefix_bit_not_native_on_int() {
	// `~x` on a plain `int` stays a native JS bitwise-not — no impl in scope,
	// `BuiltinEager` resolution.
	assert_eq!(run("func f(x: int): int = ~x", "f(new NInt(5))"), "-6");
	assert_eq!(run("func f(x: int): int = ~x", "f(new NInt(0))"), "-1");
}

#[test]
fn runs_prefix_bit_not_overload_dispatches_to_method() {
	// `~m` on a struct with a directly-defined `BitNot.bit_not` impl actually calls
	// `.bit_not()` at runtime, componentwise bit-negating the mask (Slice 4C-a).
	let src = r#"
		interface BitNot<Output> { func bit_not(): Output }
		struct Mask(a: int, b: int)
		impl BitNot<Output = Mask> for Mask {
			func bit_not(): Mask = Mask(a = ~this.a, b = ~this.b)
		}
		func flip(m: Mask): Mask = ~m
	"#;
	assert_eq!(
		run(src, "flip(new Mask({ a: new NInt(5), b: new NInt(0) })).a"),
		"-6"
	);
	assert_eq!(
		run(src, "flip(new Mask({ a: new NInt(5), b: new NInt(0) })).b"),
		"-1"
	);
}

// ── Slice 4C-b: interface default method materialization ───────────────────

#[test]
fn runs_interface_default_dispatches_via_operator() {
	// `v1 < v2` desugars to `Comparable::less_than`, which `Vec2` never defines
	// directly — only `compare_to`. Slice 4C-b materializes `less_than`'s
	// interface-default body onto `Vec2`'s class, and `less_than` itself calls
	// `this.compare_to(other)` (another materialized/impl method) — both must
	// actually run under Node and return the right boolean.
	let src = r#"
		interface Comparable<Other> {
			func compare_to(other: Other): int
			func less_than(other: Other): boolean = this.compare_to(other) < 0
		}
		struct Vec2(x: int, y: int)
		impl Comparable<Other = Vec2> for Vec2 {
			func compare_to(other: Vec2): int = this.x - other.x
		}
		func lt(v1: Vec2, v2: Vec2): boolean = v1 < v2
	"#;
	assert_eq!(
		run(
			src,
			"lt(new Vec2({ x: new NInt(1), y: new NInt(0) }), new Vec2({ x: new NInt(2), y: new NInt(0) }))"
		),
		"true"
	);
	assert_eq!(
		run(
			src,
			"lt(new Vec2({ x: new NInt(2), y: new NInt(0) }), new Vec2({ x: new NInt(1), y: new NInt(0) }))"
		),
		"false"
	);
}

#[test]
fn runs_interface_default_explicit_call() {
	// The same materialized `less_than` default, called explicitly
	// (`v.less_than(w)`) rather than through the `<` operator. Before Slice 4C-b
	// this was a *silent* miscompile (a zero-diagnostic program whose lowered JS
	// called a method that didn't exist on the class); now it's a real method.
	let src = r#"
		interface Comparable<Other> {
			func compare_to(other: Other): int
			func less_than(other: Other): boolean = this.compare_to(other) < 0
		}
		struct Vec2(x: int, y: int)
		impl Comparable<Other = Vec2> for Vec2 {
			func compare_to(other: Vec2): int = this.x - other.x
		}
		func lt(v1: Vec2, v2: Vec2): boolean = v1.less_than(v2)
	"#;
	assert_eq!(
		run(
			src,
			"lt(new Vec2({ x: new NInt(1), y: new NInt(0) }), new Vec2({ x: new NInt(2), y: new NInt(0) }))"
		),
		"true"
	);
	assert_eq!(
		run(
			src,
			"lt(new Vec2({ x: new NInt(2), y: new NInt(0) }), new Vec2({ x: new NInt(1), y: new NInt(0) }))"
		),
		"false"
	);
}

#[test]
fn runs_interface_default_override_wins() {
	// `Vec2` overrides `less_than` directly rather than relying on the interface
	// default — the override's body must be the one that actually runs (a
	// constant `false`), not the materialized default (V1: override always wins).
	let src = r#"
		interface Comparable<Other> {
			func compare_to(other: Other): int
			func less_than(other: Other): boolean = this.compare_to(other) < 0
		}
		struct Vec2(x: int, y: int)
		impl Comparable<Other = Vec2> for Vec2 {
			func compare_to(other: Vec2): int = this.x - other.x
			func less_than(other: Vec2): boolean = false
		}
		func lt(v1: Vec2, v2: Vec2): boolean = v1 < v2
	"#;
	assert_eq!(
		run(
			src,
			"lt(new Vec2({ x: 1, y: 0 }), new Vec2({ x: 2, y: 0 }))"
		),
		"false"
	);
}

#[test]
fn runs_generic_bound_operator_with_user_override() {
	let src = r#"
		interface Comparable<Other> {
			func less_than(other: Other): boolean = true
		}
		struct Vec2(x: int)
		impl Comparable<Other = Vec2> for Vec2 {
			func less_than(other: Vec2): boolean = false
		}
		func generic_lt<T: Comparable<Other = T>>(left: T, right: T): boolean = left < right
		func demo(): boolean = generic_lt(Vec2(x = 1), Vec2(x = 2))
	"#;
	assert_eq!(run(src, "demo()"), "false");
}

// ── Slice 4C-c, Task 3: comparison/equality generics end-to-end ─────────────

#[test]
fn runs_late_pinned_adt_comparison_dispatches_at_runtime() {
	// The headline silent-miscompile probe, run for real under Node: `xs[0] <
	// xs[0]` is recorded against a still-unbound inference variable, later
	// pinned to `Vec2` by the `#[Vec2]` annotation. Before W1 this compiled to a
	// native JS `<` between two class instances (`NaN`-ish nonsense); after W1
	// it must actually call `.less_than(...)` and produce the impl's real
	// answer.
	let src = r#"
		interface Comparable<Other> { func less_than(other: Other): boolean }
		struct Vec2(x: int)
		impl Comparable<Other = Vec2> for Vec2 {
			func less_than(other: Vec2): boolean = this.x < other.x
		}
		func f(a: Vec2, b: Vec2): boolean = {
			let xs = #[a, b]
			let c = xs[0] < xs[1]
			let pin: #[Vec2] = xs
			c
		}
	"#;
	assert_eq!(
		run(
			src,
			"f(new Vec2({ x: new NInt(1) }), new Vec2({ x: new NInt(2) }))"
		),
		"true"
	);
	assert_eq!(
		run(
			src,
			"f(new Vec2({ x: new NInt(2) }), new Vec2({ x: new NInt(1) }))"
		),
		"false"
	);
}

#[test]
fn runs_native_int_and_float_comparison_unchanged() {
	// W1 leaves the concrete-primitive fast path untouched: `int`/`float`
	// comparisons still compile to a native JS `<`/`>`, not a dispatched call.
	let src = "func lt(a: int, b: int): boolean = a < b
	           func gt(a: float, b: float): boolean = a > b";
	assert_eq!(run(src, "lt(new NInt(1), new NInt(2))"), "true");
	assert_eq!(run(src, "lt(new NInt(2), new NInt(1))"), "false");
	assert_eq!(run(src, "gt(new NFloat(2.5), new NFloat(1.5))"), "true");
}

#[test]
fn user_struct_operators_dispatch_through_explicit_equals() {
	let src = r#"
		interface Equals<Other> {
			func equals(other: Other): boolean
			func not_equals(other: Other): boolean = !this.equals(other)
		}
		struct Vec2(x: int)
		impl Equals<Other = Vec2> for Vec2 { func equals(other: Vec2): boolean = true }
		func same(a: Vec2, b: Vec2): boolean = a == b
		func self_same(a: Vec2): boolean = a == a
		func different(a: Vec2, b: Vec2): boolean = a != b
		func protocol_same(a: Vec2, b: Vec2): boolean = a.equals(b)
		func protocol_different(a: Vec2, b: Vec2): boolean = a.not_equals(b)
	"#;
	assert_eq!(
		run(
			src,
			"same(new Vec2({ x: new NInt(1) }), new Vec2({ x: new NInt(1) })).v"
		),
		"true"
	);
	assert_eq!(
		run(src, "self_same(new Vec2({ x: new NInt(1) })).v"),
		"true"
	);
	assert_eq!(
		run(
			src,
			"different(new Vec2({ x: new NInt(1) }), new Vec2({ x: new NInt(2) })).v"
		),
		"false"
	);
	assert_eq!(
		run(
			src,
			"protocol_same(new Vec2({ x: new NInt(1) }), new Vec2({ x: new NInt(2) })).v"
		),
		"true"
	);
	assert_eq!(
		run(
			src,
			"protocol_different(new Vec2({ x: new NInt(1) }), new Vec2({ x: new NInt(2) })).v"
		),
		"false"
	);
}

#[test]
fn runs_enum_inherent_method_matching_this() {
	// An inherent method on an enum branches on `match (this) { .. }` (the
	// checker rejects direct `this.field` access on an enum receiver — see the
	// Slice 4D plan's investigation brief, "corrections" #2 — so matching is
	// the supported way to inspect `this` inside an enum method).
	let src = r#"
		enum Color { Red, Green, Blue }
		impl Color {
			func is_red(): boolean = match (this) {
				Red -> true,
				_ -> false,
			}
		}
		func check(c: Color): boolean = c.is_red()
	"#;
	assert_eq!(run(src, "check(Color.Red)"), "true");
	assert_eq!(run(src, "check(Color.Green)"), "false");
}

#[test]
fn runs_enum_method_reads_field_variant_payload_via_match() {
	// A method reads a field variant's payload by matching `this`, working on
	// both the field variant and the nullary variant of the same enum.
	let src = r#"
		enum Opt { Some(value: int), None }
		impl Opt {
			func unwrap_or(fallback: int): int = match (this) {
				Some(value) -> value,
				None -> fallback,
			}
		}
	"#;
	assert_eq!(run(src, "Opt.Some({ value: 42 }).unwrap_or(0)"), "42");
	assert_eq!(run(src, "Opt.None.unwrap_or(0)"), "0");
}

#[test]
fn runs_enum_operator_overload_dispatches_to_method() {
	// `a + b` on an enum dispatches to `.plus(...)` (a `UserImpl` resolution),
	// exactly like the struct case, now that enums can carry methods.
	let src = r#"
		interface Plus<Other, Output> { func plus(other: Other): Output }
		enum Color { Red, Green }
		impl Plus<Other = Color, Output = Color> for Color {
			func plus(other: Color): Color = Red
		}
		func add(a: Color, b: Color): Color = a + b
	"#;
	assert_eq!(
		run(src, "add(Color.Red, Color.Green) === Color.Red"),
		"true"
	);
}

#[test]
fn runs_enum_interface_default_method() {
	// An empty `impl Describe for Color {}` materializes the interface's
	// default-bodied method onto the enum, callable like any other method.
	let src = r#"
		interface Describe { func label(): int = 1 }
		enum Color { Red, Green }
		impl Describe for Color { }
		func d(c: Color): int = c.label()
	"#;
	assert_eq!(run(src, "d(Color.Red)"), "1");
	assert_eq!(run(src, "d(Color.Green)"), "1");
}

#[test]
fn runs_enum_with_methods_preserves_tag_identity() {
	// The Slice 2C tag-identity value ABI must not change for a methodful enum:
	// a constructed field variant still shares the factory's own `[TAG]`, and
	// distinct variants still have distinct tags — while methods are also
	// callable on both variant shapes.
	let src = r#"
		enum A { X(n: int), Y }
		impl A {
			func f(): int = 0
		}
	"#;
	let tag = "Symbol.for('nymph.tag')";
	assert_eq!(
		run(src, &format!("A.X({{ n: 1 }})[{tag}] === A.X[{tag}]")),
		"true"
	);
	assert_eq!(run(src, &format!("A.X[{tag}] === A.Y[{tag}]")), "false");
	assert_eq!(run(src, "A.X({ n: 1 }).f()"), "0");
	assert_eq!(run(src, "A.Y.f()"), "0");
}

#[test]
fn compile_reports_check_errors() {
	// A type error surfaces as diagnostics, not JS.
	let result = nymph_compiler::compile("func f(): int = true", "test");
	assert!(result.is_err(), "type error should not produce JS");
}

#[test]
fn compile_produces_runnable_js() {
	let result = nymph_compiler::compile("func double(n: int): int = n * 2", "test");
	assert!(
		result.is_ok(),
		"well-typed program should compile: {result:?}"
	);
}

// ── Slice 4E: `return`, let-shadowing, module lets ──────────────────────────

#[test]
fn runs_early_return_with_value_inside_a_statement_position_if() {
	// The corpus `abs` shape: an early `return n` inside a statement-position
	// `if`, falling through to the trailing expression otherwise (Slice 4E, Y1).
	let src = r#"
		func abs(n: int): int = {
			if (n >= 0) { return n }
			0 - n
		}
	"#;
	assert_eq!(run(src, "abs(new NInt(5))"), "5");
	assert_eq!(run(src, "abs(new NInt(-3))"), "3");
	assert_eq!(run(src, "abs(new NInt(0))"), "0");
}

#[test]
fn runs_bare_return_in_a_void_function() {
	let src = r#"
		func noop(): void = {
			return
		}
	"#;
	// A `void` function still has SOME js return value (`undefined`) — assert via
	// a driver call that only checks it doesn't throw.
	assert_eq!(run(src, "(noop(), 'ok')"), "ok");
}

#[test]
fn runs_return_inside_a_statement_position_match() {
	// A braced match-arm body (`-> { return .. }`) in a STATEMENT-position match
	// (not a subexpression) emits directly — the whole `match` stays in
	// `block_stmt`, never wrapped in an IIFE, so the `return` inside targets the
	// enclosing function correctly.
	let src = r#"
		func classify(n: int): int = {
			match (n) {
				0 -> { return 100 },
				_ -> { },
			}
			n * 2
		}
	"#;
	assert_eq!(run(src, "classify(new NInt(0))"), "100");
	assert_eq!(run(src, "classify(new NInt(5))"), "10");
}

#[test]
fn return_inside_a_subexpression_position_match_arm_targets_the_function() {
	// A braced match-arm body used as a SUBEXPRESSION (here, a `let` initializer)
	// is wrapped in an IIFE by emit. The private callable completion carries its
	// return across that generated boundary to the enclosing function.
	let src = r#"
		func f(n: int): int = {
			let x = match (n) {
				0 -> { return 7 },
				_ -> n,
			}
			x
		}
	"#;
	assert_eq!(run(src, "f(new NInt(0))"), "7");
	assert_eq!(run(src, "f(new NInt(3))"), "3");
}

#[test]
fn runs_same_scope_let_shadow_computes_using_the_prior_binding() {
	// `let x = 1; let x = x + 1; x * 10` — the redeclaration renames in emitted
	// JS (avoiding a `SyntaxError: Identifier 'x' has already been declared`),
	// and its RHS reads the PRIOR `x` (Slice 4E, Y2).
	let src = r#"
		func f(): int = {
			let x = 1
			let x = x + 1
			x * 10
		}
	"#;
	assert_eq!(run(src, "f()"), "20");
}

#[test]
fn runs_triple_same_scope_let_shadow() {
	let src = r#"
		func f(): int = {
			let x = 1
			let x = x + 1
			let x = x * 10
			x + 1
		}
	"#;
	assert_eq!(run(src, "f()"), "21");
}

#[test]
fn runs_nested_block_shadow_keeps_both_values_distinct() {
	// A nested block's `let x` shadows the outer `x` inside its own JS scope
	// without any rename needed; the outer `x` is unaffected once the branch
	// exits.
	let src = r#"
		func f(): int = {
			let x = 1
			let y = if (true) { let x = 5 x * 2 } else { 0 }
			x + y
		}
	"#;
	assert_eq!(run(src, "f()"), "11"); // outer x=1, y = 5*2=10 -> 11
}

#[test]
fn runs_shadowed_name_inside_a_method_body() {
	// A body `let` reusing a PARAM's name (same merged JS scope) also needs the
	// rename to avoid a JS redeclaration error.
	let src = r#"
		struct Counter(n: int)
		impl Counter {
			func bump(n: int): int = {
				let n = n + this.n
				n
			}
		}
	"#;
	assert_eq!(
		run(src, "new Counter({ n: new NInt(10) }).bump(new NInt(5))"),
		"15"
	);
}

#[test]
fn runs_top_level_let_referenced_by_a_function() {
	let src = r#"
		let answer = 42
		func f(): int = answer
	"#;
	assert_eq!(run(src, "f()"), "42");
}

#[test]
fn runs_two_top_level_lets_where_the_second_references_the_first() {
	let src = r#"
		let base = 10
		let total = base + 5
		func f(): int = total
	"#;
	assert_eq!(run(src, "f()"), "15");
}

#[test]
fn runs_top_level_let_referencing_a_function_result() {
	let src = r#"
		let r = f2()
		func f2(): int = 9
		func f(): int = r
	"#;
	assert_eq!(run(src, "f()"), "9");
}

#[test]
fn runs_nested_block_shadow_that_reads_the_outer_binding() {
	// The exact reported hazard: a nested block's `let i` redeclares the outer
	// `i` AND its own initializer reads that outer `i` (`let i = i + 100`).
	// Without the Y2 fix, both bindings would emit as the identical JS
	// identifier `i`, and JS's block-scope hoisting (TDZ) would make the inner
	// initializer read the not-yet-initialized inner `i` instead of the outer
	// one, throwing `ReferenceError: Cannot access 'i' before initialization`.
	let src = r#"
		func f(): int = {
			let i = 1
			let r = { let i = i + 100 i }
			r
		}
	"#;
	assert_eq!(run(src, "f()"), "101");
}

#[test]
fn runs_top_level_let_referencing_a_later_let() {
	// `a`'s initializer directly names `b`, which is declared LATER in source —
	// naive source-order emission throws a TDZ `ReferenceError` under Node.
	let src = r#"
		let a = b + 1
		let b = 10
		func f(): int = a
	"#;
	assert_eq!(run(src, "f()"), "11");
}

#[test]
fn runs_top_level_let_via_a_function_reading_a_later_let() {
	// `a`'s initializer calls `g`, whose body reads `b` — a top-level `let`
	// declared textually AFTER both `a` and `g`. Function declarations hoist,
	// but `b`'s own `const` line executing only AFTER `a`'s means calling `g()`
	// as part of `a`'s initializer reads `b` while still in its TDZ, unless the
	// lets are reordered.
	let src = r#"
		let a = g()
		func g(): int = b
		let b = 5
	"#;
	assert_eq!(run(src, "a"), "5");
}

#[test]
fn runs_top_level_let_via_an_attached_method_reading_a_later_let() {
	let src = r#"
		struct Reader { func read(): int = later }
		let result = Reader().read()
		let later = 5
	"#;
	assert_eq!(run(src, "result"), "5");
}

#[test]
fn runs_top_level_let_via_a_static_method_reading_a_later_let() {
	let src = r#"
		struct Reader { namespace func read(): int = later }
		let result = Reader.read()
		let later = 5
	"#;
	assert_eq!(run(src, "result"), "5");
}

#[test]
fn top_level_initializer_self_cycles_are_compile_errors() {
	let result = nymph_compiler::compile("let value: int = value", "test");
	let diagnostics = result.expect_err("self-cycle must not produce JavaScript");
	assert!(
		diagnostics
			.iter()
			.any(|diagnostic| diagnostic.message.contains("InitializerCycle")),
		"{diagnostics:?}"
	);
}

#[test]
fn function_mediated_top_level_initializer_self_cycles_are_compile_errors() {
	let src = "let value: int = read()\nfunc read(): int = value";
	let result = nymph_compiler::compile(src, "test");
	let diagnostics = result.expect_err("function-mediated self-cycle must not produce JavaScript");
	assert!(
		diagnostics
			.iter()
			.any(|diagnostic| diagnostic.message.contains("InitializerCycle")),
		"{diagnostics:?}"
	);
}

#[test]
fn immutable_closure_initializer_calls_are_ordered_transitively() {
	let src = "let callback = () -> later\nfunc apply(callback: () -> int): int = callback()\nlet result = apply(callback)\nlet later = 5";
	assert_eq!(run(src, "result"), "5");
}

#[test]
fn closure_mediated_top_level_initializer_cycles_are_compile_errors() {
	let src = "let callback = () -> result\nlet result: int = callback()";
	let result = nymph_compiler::compile(src, "test");
	let diagnostics = result.expect_err("closure-mediated self-cycle must not produce JavaScript");
	assert!(
		diagnostics
			.iter()
			.any(|diagnostic| diagnostic.message.contains("InitializerCycle")),
		"{diagnostics:?}"
	);
}

#[test]
fn top_level_initializer_is_evaluated_once_after_closure_ordering() {
	let src = r#"
		let callback = () -> later
		let result = callback()
		let later = 1
		func observed(): #(int, int) = #(result, result)
	"#;
	let js = compile(src);
	assert_eq!(
		js.matches("const result = nymphActivate(callback").count(),
		1,
		"{js}"
	);
	assert_eq!(
		run_js(js, "JSON.stringify(nymphTestValue(observed()))"),
		"[1,1]"
	);
}

#[test]
fn stdlib_sort_initializers_run_through_external_leaf_calls() {
	std::thread::Builder::new()
		.stack_size(8 * 1024 * 1024)
		.spawn(stdlib_sort_initializers_run_through_external_leaf_calls_inner)
		.unwrap()
		.join()
		.unwrap();
}

fn stdlib_sort_initializers_run_through_external_leaf_calls_inner() {
	let src = r#"
		let sorted = #[3, 1, 2].sort()
		let descending = #[1, 3, 2].sort_by((left, right) ->
			if (left > right) { Order.LessThan }
			else if (left < right) { Order.GreaterThan }
			else { Order.Equal }
		)
		func observed(): #(#[int], #[int]) = #(sorted, descending)
	"#;
	assert_eq!(
		run(src, "JSON.stringify(nymphTestValue(observed()))"),
		"[[1,2,3],[3,2,1]]"
	);
}

#[test]
fn interpolation_runtime_dispatch_in_an_initializer_is_a_compile_error() {
	let src = "let rendered = \"value: ${1}\"";
	let result = nymph_compiler::compile(src, "test");
	let diagnostics = result.expect_err("opaque display dispatch must not produce JavaScript");
	assert!(
		diagnostics
			.iter()
			.any(|diagnostic| diagnostic.message.contains("UnresolvedInitializerCall")),
		"{diagnostics:?}"
	);
}

#[test]
fn top_level_initializer_order_uses_exact_bindings_not_shadowed_parameter_names() {
	let src = r#"
		let a = fa(1)
		let b = fb(2)
		func fa(b: int): int = b
		func fb(a: int): int = a
		func sum(): int = a + b
	"#;
	assert_eq!(run(src, "sum()"), "3");
}

#[test]
fn runs_top_level_lets_via_mutually_recursive_functions() {
	// `f` and `g` call each other (mutual recursion). `f`'s nested call to `g`
	// means the single-pass memoized DFS resolver hits `f` again while it's
	// still `in_progress`, truncates that back-edge to `{}`, and PERMANENTLY
	// memoizes `f`'s transitive let-deps as `{d}` — missing `c` and `z`, which
	// are only reachable through `g`. `ef` (which calls `f`) then gets ordered
	// after `d` but before `c`/`z`, and the emitted JS throws a TDZ
	// `ReferenceError: Cannot access 'c' before initialization`. The correct
	// answer: `f(1)` -> `g(0)` -> `c` -> `z + 10` -> `1 + 10` -> `11`.
	let src = r#"
		func f(n: int): int = if (n <= 0) { d } else { g(n - 1) }
		func g(n: int): int = if (n <= 0) { c } else { f(n - 1) }
		let ef = f(1)
		let c = z + 10
		let d = 100
		let z = 1
	"#;
	assert_eq!(run(src, "ef"), "11");
}

#[test]
fn activation_machine_reuses_one_frame_for_deep_direct_and_mutual_tail_calls() {
	let direct = r#"
		func descend(n: int): int = if (n <= 0) { n } else { descend(n - 1) }
	"#;
	assert_eq!(run(direct, "descend(new NInt(100000n))"), "0");

	let mutual = r#"
		func even(n: int): boolean = if (n <= 0) { true } else { odd(n - 1) }
		func odd(n: int): boolean = if (n <= 0) { false } else { even(n - 1) }
	"#;
	assert_eq!(run(mutual, "even(new NInt(100000n))"), "true");
}

#[test]
fn activation_machine_handles_deep_generic_match_and_dynamic_tail_transfers() {
	let generic_match = r#"
		func retain<T>(value: T, n: int): T = match (n) {
			0 -> value,
			_ -> retain(value, n - 1),
		}
	"#;
	assert_eq!(
		run(generic_match, "retain(new NInt(42n), new NInt(100000n))"),
		"42"
	);

	let dynamic = r#"
		func invoke(next: (int) -> int, n: int): int = next(n)
		func descend(n: int): int = if (n <= 0) { n } else { invoke(descend, n - 1) }
	"#;
	assert_eq!(run(dynamic, "descend(new NInt(100000n))"), "0");
}

#[test]
fn activation_machine_pushes_non_tail_calls_and_preserves_the_result() {
	let src = r#"
		func identity(n: int): int = n
		func add_one(n: int): int = identity(n) + 1
	"#;
	assert_eq!(run(src, "add_one(new NInt(41n))"), "42");
}

#[test]
fn runs_top_level_lets_via_a_three_function_mutual_recursion_cycle() {
	// `f -> g -> h -> f`, a three-function cycle, with the let-deps spread
	// across all three (`x` only reachable via `f`, `y` only via `g`, `z` only
	// via `h`). This exercises the fixpoint converging over a cycle longer than
	// two functions: `r` (which calls `f`) must land after `x`, `y`, AND `z`.
	// `f(2)` -> `g(1)` -> `h(0)` -> `z` -> `3`.
	let src = r#"
		func f(n: int): int = if (n <= 0) { x } else { g(n - 1) }
		func g(n: int): int = if (n <= 0) { y } else { h(n - 1) }
		func h(n: int): int = if (n <= 0) { z } else { f(n - 1) }
		let r = f(2)
		let x = 1
		let y = 2
		let z = 3
	"#;
	assert_eq!(run(src, "r"), "3");
}

// ── Slice 4E follow-up: `return` inside an UNBRACED if/while branch ─────────

#[test]
fn runs_bare_return_as_an_unbraced_if_then_branch() {
	let src = r#"
		func f(n: int): int = {
			if (n < 0) return 0 - n
			n
		}
	"#;
	assert_eq!(run(src, "f(new NInt(-3))"), "3");
	assert_eq!(run(src, "f(new NInt(3))"), "3");
}

// ── Slice 4G: call-site bound enforcement ───────────────────────────────────
//
// A bound-satisfying generic call, both spellings (declared generic and the
// `impl Trait` param sugar), still checks and runs correctly under Node —
// closing the soundness hole (a non-implementing argument crashing at JS
// runtime) must not regress the accepting path.

#[test]
fn runs_bound_satisfying_call_both_spellings() {
	let src = r#"
		interface Area { func area(): int }
		struct Square(side: int)
		impl Area for Square { func area(): int = this.side * this.side }
		func measure_explicit<T: Area>(shape: T): int = shape.area()
		func measure_sugar(shape: Area): int = shape.area()
		func total(s: Square): int = measure_explicit(s) + measure_sugar(s)
	"#;
	assert_eq!(run(src, "total(new Square({ side: new NInt(4) }))"), "32");
}

// ── Slice 4H: string expressions ─────────────────────────────────────────────

#[test]
fn runs_a_plain_string_literal() {
	let out = run(r#"func greet(): string = "hello""#, "greet()");
	assert_eq!(out, "hello");
}

#[test]
fn runs_string_escapes() {
	// `\n`, `\"`, `\\` all cook correctly and survive to Node's stdout.
	let out = run(r#"func f(): string = "a\nb\"c\\d""#, "f()");
	assert_eq!(out, "a\nb\"c\\d");
}

#[test]
fn runs_string_interpolation_with_a_string_interpoland() {
	let src = r#"func greet(name: string): string = "Hello, ${name}!""#;
	assert_eq!(run(src, r#"greet("World")"#), "Hello, World!");
}

#[test]
fn runs_string_interpolation_with_a_non_string_interpoland() {
	// An `int` interpoland must stringify via JS's own `+` coercion, matching
	// ordinary number-to-string formatting.
	let src = r#"func f(n: int): string = "n=${n}!""#;
	assert_eq!(run(src, "f(5)"), "n=5!");
}

#[test]
fn runs_string_equality() {
	// `==` on strings dispatches as `BuiltinEager` to native JS `===`.
	let src = "func eq(a: string, b: string): boolean = a == b";
	assert_eq!(
		run(src, r#"eq(new NString("x"), new NString("x"))"#),
		"true"
	);
	assert_eq!(
		run(src, r#"eq(new NString("x"), new NString("y"))"#),
		"false"
	);
}

#[test]
fn runs_string_concatenation() {
	let src = "func cat(a: string, b: string): string = a + b";
	assert_eq!(
		run(src, r#"cat(new NString("foo"), new NString("bar"))"#),
		"foobar"
	);
}

#[test]
fn runs_pipe_chain_applies_functions_left_to_right() {
	// DD1: `|>` lowers structurally to a `Call`; chained pipes are left-assoc, so
	// `10 |> double |> inc` is `inc(double(10))`, not `double(inc(10))`.
	let src = r#"
		func double(x: int): int = x * 2
		func inc(x: int): int = x + 1
		func f(): int = 10 |> double |> inc
	"#;
	assert_eq!(run(src, "f()"), "21");
}

#[test]
fn runs_user_contains_impl_dispatches_in_and_not_in() {
	// DD2: `a in c` / `a !in c` dispatch to `c.contains(a)` / `c.not_contains(a)` —
	// the RHS collection is the receiver, swapped from every other operator.
	let src = r#"
		interface Contains<Item> {
			func contains(item: Item): boolean
			func not_contains(item: Item): boolean
		}
		struct Bag(n: int)
		impl Contains<Item = int> for Bag {
			func contains(item: int): boolean = item == this.n
			func not_contains(item: int): boolean = item != this.n
		}
		func has(b: Bag, x: int): boolean = x in b
		func lacks(b: Bag, x: int): boolean = x !in b
	"#;
	assert_eq!(
		run(src, "has(new Bag({ n: new NInt(5) }), new NInt(5))"),
		"true"
	);
	assert_eq!(
		run(src, "has(new Bag({ n: new NInt(5) }), new NInt(6))"),
		"false"
	);
	assert_eq!(
		run(src, "lacks(new Bag({ n: new NInt(5) }), new NInt(6))"),
		"true"
	);
	assert_eq!(
		run(src, "lacks(new Bag({ n: new NInt(5) }), new NInt(5))"),
		"false"
	);
}

#[test]
fn in_operator_never_emits_native_js_in() {
	// Codegen invariant: JS `in` is key-membership, wrong for a Nymph `contains`
	// dispatch — it must never appear in the emitted output for a user `Contains`
	// impl, only activation-machine member dispatch.
	let src = r#"
		interface Contains<Item> { func contains(item: Item): boolean }
		struct Bag(n: int)
		impl Contains<Item = int> for Bag {
			func contains(item: int): boolean = item == this.n
		}
		func has(b: Bag, x: int): boolean = x in b
	"#;
	let js = compile(src);
	assert!(
		js.contains(".contains,") && js.contains("return nymphTailCall("),
		"expected activation-machine `contains` dispatch in emitted JS:\n{js}"
	);
	assert!(
		!js.contains(" in "),
		"emitted JS must never contain a native `in` operator:\n{js}"
	);
}

#[test]
fn runs_user_unwrap_impl_dispatches_eagerly() {
	// DD3 (corrected): Nymph has no optional runtime representation, so `??`
	// always dispatches to an ordinary eager `recv.unwrap(fallback)` call.
	let src = r#"
		interface Unwrap<Output> { func unwrap(default: Output): Output }
		struct MaybeInt(present: boolean, value: int)
		impl Unwrap<Output = int> for MaybeInt {
			func unwrap(fallback: int): int = if (this.present) { this.value } else { fallback }
		}
		func get(m: MaybeInt, d: int): int = m ?? d
	"#;
	assert_eq!(
		run(
			src,
			"get(new MaybeInt({ present: { v: true }, value: { v: 7 } }), { v: 99 })"
		),
		"7"
	);
	assert_eq!(
		run(
			src,
			"get(new MaybeInt({ present: { v: false }, value: { v: 7 } }), { v: 99 })"
		),
		"99"
	);
}

#[test]
fn unwrap_never_emits_native_js_nullish_coalescing() {
	// Pinning the eager-vs-short-circuit distinction at the JS-source level: a
	// user `Unwrap` overload uses activation member dispatch, never native JS
	// `??` (which would test null/undefined; Nymph values are never
	// null/undefined-based, so a native `??` here would just silently never
	// fire).
	let src = r#"
		interface Unwrap<Output> { func unwrap(default: Output): Output }
		struct MaybeInt(present: boolean, value: int)
		impl Unwrap<Output = int> for MaybeInt {
			func unwrap(fallback: int): int = if (this.present) { this.value } else { fallback }
		}
		func get(m: MaybeInt, d: int): int = m ?? d
	"#;
	let js = compile(src);
	assert!(
		js.contains(".unwrap,") && js.contains("return nymphTailCall("),
		"the user Unwrap expression must use activation member dispatch:\n{js}"
	);
}

// ── Slice 4J: `namespace func` statics, `mut func` methods ─────────────────

#[test]
fn runs_struct_namespaced_static_called_from_nymph() {
	// `Type.func(args)` inside a Nymph body lowers structurally (a `Field`
	// callee, zero call-site changes) and the DECLARATION now lands as a JS
	// `static` class method.
	let src = r#"
		struct Point(x: int, y: int) {
			namespace func at(v: int): Point = Point(x = v, y = v)
		}
		func make(v: int): int = Point.at(v).x
	"#;
	assert_eq!(run(src, "make(5)"), "5");
}

#[test]
fn runs_struct_namespaced_static_called_from_a_js_driver() {
	// The emitted `static` method is a real, externally-callable JS API, not
	// just reachable from lowered Nymph call sites.
	let src = r#"
		struct Point(x: int, y: int) {
			namespace func at(v: int): Point = Point(x = v, y = v)
		}
	"#;
	assert_eq!(run(src, "Point.at(7).x"), "7");
}

#[test]
fn runs_top_level_inherent_statics_for_struct_enum_and_generics() {
	let src = r#"
		impl Point { namespace func at(v: int): Point = Point(x = v) }
		struct Point(x: int)
		enum Choice<T> { Some(value: T), None }
		impl<T> Choice<T> { namespace func wrap(value: T): Choice<T> = Choice.Some(value = value) }
		func result(): int = {
			let number_choice: Choice<int> = Choice.wrap(5)
			let flag_choice: Choice<boolean> = Choice.wrap(true)
			let number = match (number_choice) { Some(value) -> value, None -> 0 }
			let flag = match (flag_choice) { Some(value) -> if (value) 2 else 0, None -> 0 }
			Point.at(7).x + number + flag
		}
	"#;
	assert_eq!(run(src, "result()"), "14");
	let js = compile(src);
	assert_eq!(js.matches("static at(").count(), 1, "{js}");
	assert_eq!(
		js.matches("wrap: nymphMarkCallable(function(value, $type$0) {")
			.count(),
		1,
		"{js}"
	);
}

#[test]
fn runs_enum_namespaced_static_called_from_nymph() {
	let src = r#"
		enum Color {
			Red,
			Green

			namespace func default(): self = Red
		}
		func label(c: Color): string = match (c) {
			Red -> "red",
			Green -> "green",
		}
		func main(): string = label(Color.default())
	"#;
	assert_eq!(run(src, "main()"), "red");
}

#[test]
fn runs_enum_namespaced_static_called_from_a_js_driver() {
	// The static lands as an OBJECT-level property on the enum's returned
	// object (not on `proto`, which a call through the enum name can never
	// reach) — `Color.default()` must be directly callable from raw JS and
	// return the exact `Color.Red` singleton.
	let src = r#"
		enum Color {
			Red,
			Green

			namespace func default(): self = Red
		}
	"#;
	assert_eq!(run(src, "Color.default() === Color.Red"), "true");
}

#[test]
fn runs_is_matching_and_non_matching_literal_patterns() {
	let src = "func f(x: int): boolean = x is 5";
	assert_eq!(run(src, "f(new NInt(5))"), "true");
	assert_eq!(run(src, "f(new NInt(6))"), "false");
}

#[test]
fn runs_not_is_matching_and_non_matching_literal_patterns() {
	let src = "func f(x: int): boolean = x !is 5";
	assert_eq!(run(src, "f(new NInt(5))"), "false");
	assert_eq!(run(src, "f(new NInt(6))"), "true");
}

#[test]
fn runs_is_with_a_variant_pattern_and_a_literal_field() {
	// `is` desugars with no guard support (guards are match-arm syntax, not
	// pattern syntax, so they're structurally impossible in `is`-position) — a
	// field's value is matched directly against a literal sub-pattern instead.
	let src = r#"
		enum Shape { Circle(radius: int), Square(side: int) }
		func is_big_circle(s: Shape): boolean = s is Circle(radius = 20)
	"#;
	assert_eq!(
		run(src, "is_big_circle(Shape.Circle({ radius: new NInt(20) }))"),
		"true"
	);
	assert_eq!(
		run(src, "is_big_circle(Shape.Circle({ radius: new NInt(5) }))"),
		"false"
	);
	assert_eq!(
		run(src, "is_big_circle(Shape.Square({ side: new NInt(20) }))"),
		"false"
	);
}

#[test]
fn runs_identity_cast_as_a_no_op() {
	let src = "struct P(x: int)\nfunc f(p: P): int = (p as P).x";
	assert_eq!(run(src, "f(new P({ x: 9 }))"), "9");
}

#[test]
fn runs_int_to_uint_cast_with_a_checked_range() {
	let src = "func f(n: int): uint = n as uint";
	assert_eq!(run(src, "f(new NInt(5))"), "5");
	assert_eq!(
		run(
			src,
			"(() => { try { return f(new NInt(-1)).v } catch (error) { return `${error.name}:${error.message}` } })()"
		),
		"RangeError:uint overflow"
	);
}

#[test]
fn runs_int_to_float_and_uint_to_float_casts_as_no_ops() {
	let src = "func f(n: int): float = n as float\nfunc g(n: uint): float = n as float";
	assert_eq!(run(src, "f(new NInt(5))"), "5");
	assert_eq!(run(src, "g(new NUint(5))"), "5");
}

#[test]
fn runs_float_to_int_cast_truncating_toward_zero() {
	// Math.trunc semantics: truncate toward zero, not floor.
	let src = "func f(x: float): int = x as int";
	assert_eq!(run(src, "f(new NFloat(3.7))"), "3");
	assert_eq!(run(src, "f(new NFloat(-3.7))"), "-3");
	assert_eq!(run(src, "f(new NFloat(0.0))"), "0");
}

#[test]
fn runs_float_to_uint_cast_truncates_then_checks_the_range() {
	let src = "func f(x: float): uint = x as uint";
	assert_eq!(run(src, "f(new NFloat(3.7))"), "3");
	assert_eq!(
		run(
			src,
			"(() => { try { return f(new NFloat(-3.7)).v } catch (error) { return `${error.name}:${error.message}` } })()"
		),
		"RangeError:uint overflow"
	);
}

#[test]
fn float_to_int_cast_rejects_nan_and_infinity() {
	let src = "func f(x: float): int = x as int";
	for value in ["NaN", "Infinity", "-Infinity"] {
		assert_eq!(
			run(
				src,
				&format!(
					"(() => {{ try {{ return f(new NFloat({value})).v }} catch (error) {{ return `${{error.name}}:${{error.message}}` }} }})()"
				)
			),
			"RangeError:float-to-integer conversion requires a finite value"
		);
	}
}

#[test]
fn float_to_uint_cast_rejects_nan_and_infinity() {
	let src = "func f(x: float): uint = x as uint";
	for value in ["NaN", "Infinity", "-Infinity"] {
		assert_eq!(
			run(
				src,
				&format!(
					"(() => {{ try {{ return f(new NFloat({value})).v }} catch (error) {{ return `${{error.name}}:${{error.message}}` }} }})()"
				)
			),
			"RangeError:float-to-integer conversion requires a finite value"
		);
	}
}

#[test]
fn runs_char_to_int_cast_via_code_point_of() {
	let src = "func f(c: char): int = c as int";
	assert_eq!(run(src, "f(new NChar('A'))"), "65");
	assert_eq!(run(src, "f(new NChar('0'))"), "48");
}

#[test]
fn runs_char_to_uint_and_char_to_float_casts_via_code_point_of() {
	let src = "func f(c: char): uint = c as uint\nfunc g(c: char): float = c as float";
	assert_eq!(run(src, "f(new NChar('A'))"), "65");
	assert_eq!(run(src, "g(new NChar('A'))"), "65");
}

#[test]
fn runs_char_to_int_cast_uses_code_point_not_utf16_code_unit() {
	// Docs pin `char` as a single Unicode codepoint, not a UTF-16 code unit — an
	// astral character (outside the BMP) is 2 JS UTF-16 code units, so this must
	// use `codePointAt`, never `charCodeAt` (which would return a lone surrogate).
	let src = "func f(c: char): int = c as int";
	// `call` is raw JS (appended after the compiled module), so this is a JS
	// string literal — `\u{...}` (braced) is JS's astral-codepoint escape, unlike
	// Nymph's own unbraced `\uXXXX` char-literal escape used elsewhere in `src`.
	assert_eq!(run(src, "f(new NChar('\\u{1F600}'))"), "128512");
}

#[test]
fn runs_int_to_char_and_uint_to_char_casts_via_string_from_code_point() {
	let src = "func f(n: int): char = n as char\nfunc g(n: uint): char = n as char";
	assert_eq!(run(src, "f(new NInt(65))"), "A");
	assert_eq!(run(src, "g(new NUint(65))"), "A");
}

#[test]
fn runs_float_to_char_cast_truncating_then_from_code_point() {
	let src = "func f(x: float): char = x as char";
	assert_eq!(run(src, "f(new NFloat(65.9))"), "A");
}

#[test]
fn scalar_casts_use_the_canonical_destination_box() {
	let src = "func i(n: int): int = n as int\nfunc f(n: int): float = n as float\nfunc u(n: uint): int = n as int\nfunc c(n: int): char = n as char";
	assert_eq!(run(src, "i(new NInt(5)).constructor.name"), "NInt");
	assert_eq!(run(src, "f(new NInt(5)).constructor.name"), "NFloat");
	assert_eq!(run(src, "u(new NUint(5)).constructor.name"), "NInt");
	assert_eq!(run(src, "c(new NInt(65)).constructor.name"), "NChar");
}

#[test]
fn dynamic_numeric_to_char_rejects_non_scalars_deterministically() {
	let src = "func i(n: int): char = n as char\nfunc f(n: float): char = n as char";
	for (function, value) in [
		("i", "new NInt(-1)"),
		("i", "new NInt(55296)"),
		("i", "new NInt(1114112)"),
		("f", "new NFloat(NaN)"),
		("f", "new NFloat(Infinity)"),
	] {
		let stderr = run_failure(src, &format!("{function}({value})"));
		assert!(stderr.contains("Invalid code point"), "{stderr}");
	}
}

#[test]
fn numeric_to_char_accepts_boundaries_bmp_and_astral_values() {
	let src = "func f(n: int): char = n as char";
	for (value, expected) in [
		(0, 0),
		(55295, 55295),
		(57344, 57344),
		(128512, 128512),
		(1114111, 1114111),
	] {
		assert_eq!(
			run(src, &format!("f(new NInt({value})).v.codePointAt(0)")),
			expected.to_string()
		);
	}
}

#[test]
fn runs_cast_via_a_user_into_impl() {
	let src = r#"
		interface Into<Other> { func into(): Other }
		struct P(x: int)
		impl Into<string> for P { func into(): string = "p" }
		func f(p: P): string = p as string
	"#;
	assert_eq!(run(src, "f(new P({ x: 9 }))"), "p");
}

#[test]
fn runs_cast_via_a_user_into_impl_with_a_custom_method_name() {
	// Defect 1 (critical): `check_cast` used to hardcode the dispatched method name
	// to `"into"` regardless of what the resolved `Into`-named interface actually
	// declares — silently emitting a call to a method that doesn't exist on the
	// class whenever the interface's sole method isn't literally named `into`.
	// This is the same `runs_cast_via_a_user_into_impl` scenario above, but with the
	// interface's method renamed to `convert`, run end-to-end under Node.
	let src = r#"
		interface Into<Other> { func convert(): Other }
		struct P(x: int)
		impl Into<string> for P { func convert(): string = "p" }
		func f(p: P): string = p as string
	"#;
	assert_eq!(run(src, "f(new P({ x: 9 }))"), "p");
}

#[test]
fn cast_via_into_impl_never_emits_a_native_as_keyword_or_call_to_math() {
	// Codegen invariant: a user `Into` dispatch must use activation-machine member
	// dispatch, never anything referencing the built-in scalar-cast machinery.
	let src = r#"
		interface Into<Other> { func into(): Other }
		struct P(x: int)
		impl Into<string> for P { func into(): string = "p" }
		func f(p: P): string = p as string
	"#;
	let js = compile(src);
	assert!(
		js.contains(".into,") && js.contains("return nymphTailCall("),
		"expected activation-machine `into` dispatch in emitted JS:\n{js}"
	);
	assert!(
		!js.contains("Math.trunc") && !js.contains("codePointAt") && !js.contains("fromCodePoint"),
		"emitted JS must not reference scalar-cast machinery for an `Into` dispatch:\n{js}"
	);
}

// ── Slice 4K, HH3 (Defect 2): namespaced static vs. interface-impl method ──

#[test]
fn namespaced_static_and_interface_impl_method_sharing_a_name_both_run() {
	// A `namespace func plus` static and a top-level `impl Plus … for Vec2`
	// instance method named `plus` are different JS slots (a class static vs. a
	// prototype method), so this must both check clean AND actually run both
	// call shapes correctly under Node — the checker-level non-collision alone
	// wouldn't catch a codegen-side mixup between the two lists.
	let src = r#"
		interface Plus<Other, Output> { func plus(other: Other): Output }
		struct Vec2(x: int, y: int) {
			namespace func plus(a: Vec2, b: Vec2): Vec2 = Vec2(x = a.x + b.x, y = a.y + b.y)
		}
		impl Plus<Other = Vec2, Output = Vec2> for Vec2 {
			func plus(other: Vec2): Vec2 = Vec2(x = this.x + other.x, y = this.y + other.y)
		}
		func via_static(a: Vec2, b: Vec2): Vec2 = Vec2.plus(a, b)
		func via_instance(a: Vec2, b: Vec2): Vec2 = a.plus(b)
		func via_operator(a: Vec2, b: Vec2): Vec2 = a + b
	"#;
	assert_eq!(
		run(
			src,
			"JSON.stringify(nymphTestValue(via_static(new Vec2({ x: new NInt(1), y: new NInt(2) }), new Vec2({ x: new NInt(10), y: new NInt(20) }))))"
		),
		r#"{"x":11,"y":22}"#
	);
	assert_eq!(
		run(
			src,
			"JSON.stringify(nymphTestValue(via_instance(new Vec2({ x: new NInt(1), y: new NInt(2) }), new Vec2({ x: new NInt(10), y: new NInt(20) }))))"
		),
		r#"{"x":11,"y":22}"#
	);
	assert_eq!(
		run(
			src,
			"JSON.stringify(nymphTestValue(via_operator(new Vec2({ x: new NInt(1), y: new NInt(2) }), new Vec2({ x: new NInt(10), y: new NInt(20) }))))"
		),
		r#"{"x":11,"y":22}"#
	);
}

#[test]
fn runs_bool_and_short_circuits_never_evaluating_the_rhs() {
	// `&&`/`||` are never overloadable (the language decision this test pins) and
	// always short-circuit, mirroring Rust. Proving "the RHS was never evaluated"
	// without relying on exceptions: make the RHS an infinitely-recursing function.
	// If codegen ever regressed to eagerly evaluating both operands, calling the
	// RHS blows the JS call stack, Node exits non-zero, and this test's `run`
	// helper fails loudly instead of silently mismatching a value.
	let src = "
		func recurse(): boolean = recurse()
		func f(a: boolean): boolean = a && recurse()
	";
	assert_eq!(run(src, "f(false)"), "false");
}

#[test]
fn runs_bool_or_short_circuits_never_evaluating_the_rhs() {
	// Same proof as `runs_bool_and_short_circuits_never_evaluating_the_rhs`, for `||`.
	let src = "
		func recurse(): boolean = recurse()
		func f(a: boolean): boolean = a || recurse()
	";
	assert_eq!(run(src, "f({ v: true })"), "true");
}

// ── Closures (Slice 4L) ──────────────────────────────────────────────────────

#[test]
fn runs_a_paren_closure_as_a_pipe_rhs() {
	let src = "func f(): int = 10 |> (x: int) -> x + 1";
	assert_eq!(run(src, "f()"), "11");
}

#[test]
fn runs_a_single_ident_closure_as_a_pipe_rhs() {
	let src = "func f(): int = 10 |> x -> x * 2";
	assert_eq!(run(src, "f()"), "20");
}

#[test]
fn runs_a_closure_passed_as_a_function_argument() {
	let src = "
		func apply_twice(f: (int) -> int, x: int): int = f(f(x))
		func g(): int = apply_twice((x: int) -> x * 2, 3)
	";
	assert_eq!(run(src, "g()"), "12");
}

#[test]
fn runs_a_closure_capturing_a_shadow_renamed_outer_binding() {
	// `let x = 1; let x = x + 1` renames the second binding to `x$1` in the
	// emitted JS (Slice 4E, Y2) — a closure defined afterward, reading the
	// free variable `x`, must resolve to that SAME renamed binding, not a
	// stale reference to the first `x`.
	let src = "
		func f(): int = {
			let x = 1
			let x = x + 1
			let g = () -> x
			g() * 10
		}
	";
	assert_eq!(run(src, "f()"), "20");
}

#[test]
fn runs_a_closure_inside_a_method_body_capturing_this() {
	// Arrows inherit `this` lexically — a closure built inside a method body
	// and returned, then called later from OUTSIDE the method, must still
	// read the ORIGINAL receiver's field.
	let src = "
		struct Adder(n: int) {
			func make(): (int) -> int = (x: int) -> x + this.n
		}
		func f(): int = Adder(n = 10).make()(5)
	";
	assert_eq!(run(src, "f()"), "15");
}

// ── Anonymous closure parameters (`$`, `$0`, `$1`, …) ───────────────────────

#[test]
fn runs_two_dollars_as_direct_call_args_desugared_to_one_closure() {
	// `combine($1, $0)` => `(p0, p1) => combine(p1, p0)` — swaps the two
	// arguments the closure is eventually applied to.
	let src = "
		func combine(a: int, b: int): int = a - b
		func apply2(cb: (int, int) -> int, a: int, b: int): int = cb(a, b)
		func g(): int = apply2(combine($1, $0), 10, 3)
	";
	// combine(p0, p1) = p0 - p1; applied with (a=10, b=3) swapped => combine(3, 10) = -7.
	assert_eq!(run(src, "g()"), "-7");
}

#[test]
fn runs_a_call_argument_boundary_with_no_expansion() {
	// `$0 + 1` already checks as `(int) -> int` at its smallest boundary.
	let src = "
		func apply(cb: (int) -> int, x: int): int = cb(x)
		func g(): int = apply($0 + 1, 5)
	";
	assert_eq!(run(src, "g()"), "6");
}

#[test]
fn runs_the_key_comparison_boundary_expansion_case() {
	// `$ % 2 == 0`: the smallest boundary (`$ % 2`) would be ill-typed (a
	// closure compared to an int) — the search expands to the whole
	// comparison, which checks as `(int) -> boolean`.
	let src = "
		func check_pred(cb: (int) -> boolean, x: int): boolean = cb(x)
		func is_even(): boolean = check_pred($ % 2 == 0, 4)
		func is_odd(): boolean = check_pred($ % 2 == 0, 5)
	";
	assert_eq!(run(src, "is_even()"), "true");
	assert_eq!(run(src, "is_odd()"), "false");
}

#[test]
fn runs_a_repeated_anon_param_as_one_shared_slot() {
	// `add($0, $0)`: one param used twice — arity 1, not 2.
	let src = "
		func add(a: int, b: int): int = a + b
		func apply(cb: (int) -> int, x: int): int = cb(x)
		func g(): int = apply(add($0, $0), 5)
	";
	assert_eq!(run(src, "g()"), "10");
}

#[test]
fn runs_a_bare_dollar_as_the_whole_call_argument() {
	// `inc($0)`: the smallest non-`$` enclosing expression is the `Call`
	// itself — `(p0) => inc(p0)`.
	let src = "
		func inc(a: int): int = a + 1
		func apply(cb: (int) -> int, x: int): int = cb(x)
		func g(): int = apply(inc($0), 5)
	";
	assert_eq!(run(src, "g()"), "6");
}

#[test]
fn runs_nested_dollars_at_different_depths_as_independent_boundaries() {
	// `f($0, g($1))`: `$1`'s boundary is the inner `g(...)` call, `$0`'s is
	// the outer `f(...)` call — two independent, non-overlapping closures.
	let src = "
		func g(x: int): int = x * 10
		func f(a: int, cb: (int, int) -> int): int = a + cb(a, 100)
		func apply(h: (int) -> int, x: int): int = h(x)
		func caller(): int = apply(f($0, g($1)), 5)
	";
	// outer: (p0) => f(p0, (q0, q1) => g(q1)), applied to 5:
	//   f(5, cb) = 5 + cb(5, 100) = 5 + g(100) = 5 + 1000 = 1005
	assert_eq!(run(src, "caller()"), "1005");
}

#[test]
fn runs_a_dollar_inside_an_explicit_closure_bounded_by_its_own_body() {
	// `$0` inside an explicit closure's body forms its OWN nested anon
	// closure bounded by that body, capturing the explicit closure's own
	// param `x` by reference — it does not escape to the call argument.
	let src = "
		func run_two(outer: (int) -> ((int) -> int), a: int, b: int): int = outer(a)(b)
		func g(): int = run_two((x: int) -> $0 + x, 10, 5)
	";
	// outer = (x) => ((p0) => p0 + x); outer(10) = (p0) => p0 + 10; (5) => 15.
	assert_eq!(run(src, "g()"), "15");
}

#[test]
fn runs_a_functions_own_body_as_a_closure_slot() {
	// A function's own body is exactly the same kind of slot a call argument
	// is — a zero-arg function whose body IS the anon closure itself.
	let src = "
		func check_pred(cb: (int) -> boolean, x: int): boolean = cb(x)
		func is_even(): (int) -> boolean = $ % 2 == 0
		func g(): boolean = check_pred(is_even(), 4)
	";
	assert_eq!(run(src, "g()"), "true");
}

#[test]
fn runs_a_let_initializer_as_a_closure_slot() {
	let src = "
		func apply(cb: (int) -> int, x: int): int = cb(x)
		func g(): int = {
			let double = $0 * 2
			apply(double, 21)
		}
	";
	assert_eq!(run(src, "g()"), "42");
}

#[test]
fn runs_a_constructor_field_as_a_closure_slot() {
	// `Adder(cb = $0 + 100)`: the labeled constructor argument is a
	// `check_ctor_args` slot — the closure is stored as a struct field, read
	// back out, and applied through an ordinary function.
	let src = "
		struct Adder(cb: (int) -> int)
		func apply(cb: (int) -> int, x: int): int = cb(x)
		func g(): int = {
			let a = Adder(cb = $0 + 100)
			apply(a.cb, 5)
		}
	";
	assert_eq!(run(src, "g()"), "105");
}

#[test]
fn runs_an_enum_variant_constructor_field_as_a_closure_slot() {
	// Same slot reached through `infer_variant_ctor` instead of
	// `infer_struct_ctor` — both feed the same `check_ctor_args`.
	let src = "
		enum Holder { With(cb: (int) -> int) }
		func call_cb(h: Holder, x: int): int = match (h) {
			With(cb) -> cb(x),
		}
		func g(): int = call_cb(With(cb = $0 + 100), 5)
	";
	assert_eq!(run(src, "g()"), "105");
}

// ── SS1: smart literal spread ────────────────────────────────────────────────

#[test]
fn runs_static_tuple_spreads_as_a_canonical_boxed_tuple() {
	let src = r#"
		func f(): #(int, boolean, string, uint) = #(1, ...#(), ...#(true, "x"), 2u)
	"#;
	assert_eq!(
		run(src, "JSON.stringify(nymphTestValue(f()))"),
		r#"[1,true,"x",2]"#
	);
}

#[test]
fn runs_a_list_spread_over_a_native_list_source() {
	// A native `#[T]` list source is already a JS array — `[...xs, y]`, no
	// drain.
	let src = r#"
		func f(xs: #[int]): #[int] = #[...xs, 4]
	"#;
	assert_eq!(
		run(
			src,
			"JSON.stringify(nymphTestValue(f(new NList([new NInt(1), new NInt(2), new NInt(3)]))))"
		),
		"[1,2,3,4]"
	);
}

#[test]
fn runs_a_mid_list_spread_preserves_order() {
	// `#[a, ...xs, b]` splices `xs` in-position, not just at the end.
	let src = r#"
		func f(xs: #[int]): #[int] = #[0, ...xs, 9]
	"#;
	assert_eq!(
		run(
			src,
			"JSON.stringify(nymphTestValue(f(new NList([new NInt(1), new NInt(2), new NInt(3)]))))"
		),
		"[0,1,2,3,9]"
	);
}

#[test]
fn runs_a_map_spread_merge_with_later_key_wins() {
	// `#{...m, k: v}` is a `Map` MERGE (later keys win), not object spread —
	// the literal's own `1: 100` overwrites the spread source's `1: 10`.
	let src = r#"
		func f(m: #{int: int}): #{int: int} = #{...m, 1: 100, 4: 40}
	"#;
	assert_eq!(
		run(
			src,
			"JSON.stringify(nymphTestValue(f(new Map([[new NInt(1), new NInt(10)], [new NInt(2), new NInt(20)], [new NInt(3), new NInt(30)]]))).sort(([a], [b]) => a - b))"
		),
		"[[1,100],[2,20],[3,30],[4,40]]"
	);
}

#[test]
fn runs_a_map_spread_over_a_native_list_of_pairs_source() {
	// A native `#[#(K, V)]` list source is a JS array already — it splices
	// directly into the `new Map([...])` entries with no drain, exactly like
	// the native `Map` merge case above.
	let src = r#"
		func f(): #{int: string} = {
			let pairs = #[#(1, "a"), #(2, "b")]
			#{...pairs, 9: "z"}
		}
	"#;
	assert_eq!(
		run(
			src,
			"JSON.stringify(nymphTestValue(f()).sort(([a], [b]) => a - b))"
		),
		r#"[[1,"a"],[2,"b"],[9,"z"]]"#
	);
}

#[test]
fn real_list_appended_materializes_once_and_preserves_the_source() {
	let user = r#"
		func check(): int = {
			let xs = #[1]
			let ys = xs.appended(2)
			if (xs.length() == 1u) ys[1] else 0
		}
	"#;
	assert_eq!(run(user, "check()"), "2");
}

#[test]
fn mixed_int_uint_operators_run_under_node() {
	// End-to-end for the int<->uint operator slice: mixed operators type-check
	// against the real stdlib's cross-type impls and execute as native JS. Notably
	// the arithmetic Output is the signed `int` domain (so `sub` can go negative) and
	// division is `float` (`7 / 2 == 3.5`, not integer). Since every mixed primitive
	// operator lowers `BuiltinEager`, the emitted body is a plain native operator.
	let src = "\
		func add(a: int, b: uint): int = a + b\n\
		func sub(a: int, b: uint): int = a - b\n\
		func mul(a: int, b: uint): int = a * b\n\
		func div(a: int, b: uint): float = a / b\n\
		func lt(a: int, b: uint): boolean = a < b\n\
		func eq(a: int, b: uint): boolean = a == b\n\
		func ne(a: int, b: uint): boolean = a != b";
	let js = compile(src);
	assert_eq!(run_js(js.clone(), "add(new NInt(3), new NUint(2))"), "5");
	assert_eq!(run_js(js.clone(), "sub(new NInt(2), new NUint(5))"), "-3");
	assert_eq!(run_js(js.clone(), "mul(new NInt(4), new NUint(3))"), "12");
	assert_eq!(run_js(js.clone(), "div(new NInt(7), new NUint(2))"), "3.5");
	assert_eq!(run_js(js.clone(), "lt(new NInt(3), new NUint(5))"), "true");
	assert_eq!(run_js(js.clone(), "eq(new NInt(4), new NUint(4))"), "true");
	assert_eq!(run_js(js, "ne(new NInt(4), new NUint(5))"), "true");
}

// FLIP (Gap 3, L0): `is_empty` (`this.length() == 0`) is real Nymph source,
// not `external` itself — it used to stay a loud defer because the method it
// transitively calls, `length`, WAS `external` with no JS binding anywhere
// (`body_calls_unlinked_external`'s member-call extension caught it). Now
// that `length` is a LINKED external (Gap 3, L0's one seeded registry entry —
// see `nymph_hir::linkage::REGISTRY`), `body_calls_unlinked_external` no
// longer counts it as unlinked, so `is_empty` materializes: its `this.length()`
// lowers to `HirExpr::ExternCall`, which emits a module-qualified local call
// plus a deduped import from `std/collections/list`. This can't
// is asserted here at the emitted-JS boundary; the bundle-path e2e in
// `nymph-compiler`'s `tests/std_linkage.rs` proves the same mechanism actually
// RUNS, imports resolved and all.
#[test]
fn real_list_is_empty_materializes_once_length_is_linked() {
	let user = "func check(): #(boolean, boolean) = { let xs: #[int] = #[]\n #(xs.is_empty(), #[1].is_empty()) }";
	assert_eq!(
		run(user, "JSON.stringify(nymphTestValue(check()))"),
		"[true,false]"
	);
}

// List.get resolves through the immutable list receiver tag. The bundle test
// also proves that the canonical Option crosses this external boundary.
#[test]
fn real_list_get_materializes_once_get_is_linked() {
	let user = r#"
		func check(): int = match (#[7].get(0)) {
			Some(value) -> value,
			None -> 0,
		}
	"#;
	assert_eq!(run(user, "check()"), "7");
}

// Map and list share the `get` marker, so the receiver tag must select the map adapter.
#[test]
fn real_map_get_materializes_once_get_is_linked() {
	let user = r#"
		func check(): int = match (#{1: 9}.get(1)) {
			Some(value) -> value,
			None -> 0,
		}
	"#;
	assert_eq!(run(user, "check()"), "9");
}

// FLIP (Gap 3, L3): `map.nym`'s `is_empty` (`this.size() == 0`) is
// transitively external through `size`, mirroring the list case above —
// `size` is now a LINKED (unambiguous, `receiver_tag: None`) external, so
// `body_calls_unlinked_external`'s registry subtraction no longer counts it
// as unlinked and `is_empty` materializes.
#[test]
fn real_map_is_empty_materializes_once_size_is_linked() {
	let user = "func check(): #(boolean, boolean) = { let m: #{int: int} = #{}\n #(m.is_empty(), #{1: 2}.is_empty()) }";
	assert_eq!(
		run(user, "JSON.stringify(nymphTestValue(check()))"),
		"[true,false]"
	);
}

// ── Named-type prelude method materialization: a prelude-only INSTANCE method
// on a NAMED enum receiver (`Option`/`Result`) now materializes ONTO that
// enum's own emitted class and RUNS, instead of panicking at the
// "prelude-only impl" wall above (that wall still stands for every OTHER
// unmaterializable shape — external/transitively-external collection
// intrinsics, a still-generic `GenericBound` receiver, and a genuinely
// unmaterializable body like `T.default()` through type erasure). See
// Stable compilation projects those methods onto the enum's runtime class.

/// Compile through the stable compiler session, run under Node, and return stdout.
fn run_against_real_stdlib(user_src: &str, call: &str) -> String {
	run(user_src, call)
}

#[test]
fn real_option_is_some_and_is_none_materialize_onto_the_option_class_and_run() {
	// `is_some`/`is_none` are `Option`'s own INLINE methods (`option.nym`);
	// `is_none = !this.is_some()` additionally exercises Sub-problem #1 (inner
	// dispatch): while `Option`'s class is being materialized, the `this.is_some()`
	// call inside `is_none`'s own body must resolve as a plain sibling method
	// call, not panic or re-route through the mangled-function path.
	let user = r#"
		func check_some(): #(boolean, boolean) = #(Some(1).is_some(), Some(1).is_none())
		func inspect(o: Option<int>): #(boolean, boolean) = #(o.is_some(), o.is_none())
		func check_none(): #(boolean, boolean) = inspect(None)
	"#;
	assert_eq!(
		run_against_real_stdlib(user, "JSON.stringify(nymphTestValue(check_some()))"),
		"[true,false]"
	);
	assert_eq!(
		run_against_real_stdlib(user, "JSON.stringify(nymphTestValue(check_none()))"),
		"[false,true]"
	);
}

#[test]
fn real_option_match_over_a_materialized_enum_runs() {
	// A `match` over a demand-materialized `Option` still works exactly like
	// any other enum: `materialize_referenced_prelude_enums`'s fixed point
	// materializes the CLASS itself (variants included) regardless of demand,
	// and `Some`/`None` are its ordinary variant bindings.
	let user = r#"
		func check_some(): int = match (Some(42)) {
			Some(value) -> value,
			None -> 0,
		}
		func inspect(o: Option<int>): int = match (o) { Some(value) -> value, None -> 0 }
		func check_none(): int = inspect(None)
	"#;
	assert_eq!(run_against_real_stdlib(user, "check_some()"), "42");
	assert_eq!(run_against_real_stdlib(user, "check_none()"), "0");
}

#[test]
fn real_option_map_materializes_and_runs() {
	// `map` is another of `Option`'s own inline methods, this time taking a
	// closure argument (the sibling closure-lowering track's already-landed
	// machinery — untouched by this fix) and itself constructing a `Some` via
	// `VariantNew` inside the materialized body.
	let user = r#"
		func check(): int = match (Some(1).map((x) -> x + 1)) { Some(value) -> value, None -> 0 }
	"#;
	assert_eq!(run_against_real_stdlib(user, "check()"), "2");
}

#[test]
fn real_option_unwrap_via_the_unwrap_interface_materializes_onto_the_class() {
	// `unwrap(default)` is NOT one of `Option`'s own inline methods — it comes
	// from the TOP-LEVEL `impl<T> Unwrap<Output = T> for Option<T>` block in
	// `option.nym` (Sub-problem #4: a top-level impl targeting a named prelude
	// enum, never fed to `collect_adt_methods`'s inline-member pass at all).
	// Its own parameter is literally named `default` (a JS reserved word) —
	// exercising `declare`'s reserved-word rename fix too.
	let user = r#"
		func check_some(): int = Some(7).unwrap(0)
		func inspect(o: Option<int>): int = o.unwrap(9)
		func check_none(): int = inspect(None)
	"#;
	assert_eq!(run_against_real_stdlib(user, "check_some()"), "7");
	assert_eq!(run_against_real_stdlib(user, "check_none()"), "9");
}

#[test]
fn real_result_ok_and_err_cross_materialize_option_from_convert_nym() {
	// `ok`/`err` live in `convert.nym`'s top-level `impl<T, E> Result<T, E>`
	// block (not `result.nym` itself), and each builds an `Option` variant —
	// exercising cross-enum fixed-point materialization: `Result` is
	// materialized first (demanded by `res.ok()`/`res.err()`), and `Option`
	// only becomes referenced as a SIDE EFFECT of lowering `ok`'s/`err`'s own
	// bodies (`Option.Some(..)`/`Option.None`), which
	// `materialize_referenced_prelude_enums`'s fixed-point loop must notice on
	// a LATER round (the `VariantNew` gap fix — collecting `enum_name` off a
	// `VariantNew`, not just a bare `VariantRef` — matters here: `ok`/`err`'s
	// bodies never write a bare `None`/`Some` reference, only `Option.Some(..)`/
	// `Option.None` qualified constructions).
	//
	// Both checks call `is_some()`/pattern-match `Option` from WITHIN the
	// compiled Nymph program (not the JS driver text): `Option`'s `is_some`
	// method is itself only demand-materialized because a COMPILED call site
	// asks for it — a JS driver calling `.is_some()` directly would never
	// route through `try_materialize_prelude_dispatch` at all, so it
	// wouldn't actually exercise (or need) this slice's mechanism.
	let user = r#"
		func inspect_ok(r: Result<int, string>): boolean = r.ok().is_some()
		func ok_is_some(): boolean = inspect_ok(Ok(5))
		func inspect_err(r: Result<int, string>): string = match (r.err()) {
			Some(value) -> value,
			None -> "no error",
		}
		func err_value(): string = inspect_err(Error(error = "boom"))
	"#;
	assert_eq!(run_against_real_stdlib(user, "ok_is_some()"), "true");
	assert_eq!(run_against_real_stdlib(user, "err_value()"), "boom");
}

#[test]
fn real_option_map_or_default_lowers_its_hidden_canonical_type_object_dispatch() {
	// `Option`'s own `map_or_default` (`option.nym`) calls `R.default()`.
	// Activation lowering must preserve the receiverless generic dispatch and
	// its hidden ABI. End-to-end stable-project execution is covered by
	// `core_prelude_ambient::default_generic_bound_executes_through_the_ambient_canonical_type_object`.
	let user = r#"
		func get(o: Option<int>): int = o.map_or_default((x) -> x)
	"#;
	let js = compile(user);
	assert!(
		js.contains(".default,") && js.contains("return nymphTailCall("),
		"{js}"
	);
	assert!(
		js.contains("map_or_default: nymphMarkCallable(function(f, $type$0)"),
		"{js}"
	);
	assert!(
		js.contains("$m9$Option.$nymph$type.map_or_default") && js.contains("return nymphTailCall("),
		"{js}"
	);
}

#[test]
fn real_range_contains_runs_generic_comparison_dispatch() {
	let user = r#"
		func in_range(x: int): boolean = {
			let r = Range(start = 0, end = 5)
			r.contains(x)
		}
	"#;
	assert_eq!(run(user, "in_range(new NInt(3))"), "true");
	assert_eq!(run(user, "in_range(new NInt(7))"), "false");
}

#[test]
fn positional_variant_subpatterns_on_single_field_constructors_run() {
	// A single-field constructor may take a bare positional sub-pattern: a nested
	// variant WITH a field (`Holds(One(value))`) and a nested nullary variant
	// (`Holds(Zero)`), each matched against the sole field by position, not by name.
	// (Values are built inside no-arg wrappers — the harness appends `call` as raw JS,
	// where Nymph named-arg construction isn't valid.)
	let src = "enum Inner { One(value: int), Zero }\nenum Wrap { Holds(inner: Inner) }\nfunc classify(w: Wrap): int = match (w) {\n\tHolds(One(value)) -> value,\n\tHolds(Zero) -> 0,\n}\nfunc nested(): int = classify(Wrap.Holds(inner = Inner.One(value = 7)))\nfunc empty(): int = classify(Wrap.Holds(inner = Inner.Zero))";
	assert_eq!(run(src, "nested()"), "7");
	assert_eq!(run(src, "empty()"), "0");
}

#[test]
fn positional_literal_and_binding_subpatterns_run() {
	// A literal (`One(5)`) and a bare binding whose name is NOT the field name
	// (`One(x)` binds the sole `value` field to `x`) both work positionally.
	let src = "enum Inner { One(value: int), Zero }\nfunc pick(b: Inner): int = match (b) {\n\tOne(5) -> 500,\n\tOne(x) -> x,\n\tZero -> 0,\n}\nfunc lit(): int = pick(Inner.One(value = 5))\nfunc bound(): int = pick(Inner.One(value = 8))\nfunc nil(): int = pick(Inner.Zero)";
	assert_eq!(run(src, "lit()"), "500");
	assert_eq!(run(src, "bound()"), "8");
	assert_eq!(run(src, "nil()"), "0");
}

#[test]
fn pattern_binding_exposes_whole_value_and_nested_captures() {
	let src = "func capture(value: #(int, int)): #(int, int, int) = match (value) {\n\twhole = #(left, right) -> #(whole[0], left, right),\n}\nfunc captured(): #(int, int, int) = capture(#(4, 7))";
	assert_eq!(run(src, "captured()"), "[ 4, 4, 7 ]");
}

#[test]
fn immutable_state_loops_replace_simultaneously_and_capture_each_iteration() {
	let src = r#"
func simultaneous(): #(int, int) = loop (
  let left = 1
  let right = 2
  let step = 0
) {
  if (step == 2) { break #(left, right) }
  continue(left = right, right = left, step = step + 1)
}
func captured(): int = loop (
  let value = 0
  let saved: () -> int = () -> 99
  let step = 0
) {
  if (step == 2) { break saved() }
  continue(saved = () -> value, value = value + 1, step = step + 1)
}
func deep(): int = loop (let value = 0) {
  if (value == 10000) { break value }
  continue(value = value + 1)
}
func labeled(): int = loop@outer (let value = 0) {
	if (value == 3) { break@outer value }
	continue@outer(value = value + 1)
}
"#;
	assert_eq!(run(src, "simultaneous()"), "[ 1, 2 ]");
	assert_eq!(run(src, "captured()"), "1");
	assert_eq!(run(src, "deep()"), "10000");
	assert_eq!(run(src, "labeled()"), "3");
}

#[test]
fn immutable_state_loop_managed_replacements_execute_end_to_end() {
	let src = r#"
struct Resource
impl Close<!()> for Resource {
	func close(): void = {}
}
func managed(): int = loop (let use resource = Resource(), let step = 0) {
    let use body = Resource()
    if (step == 1) { break step }
    continue(resource = Resource(), step = step + 1)
  }
"#;
	assert_eq!(run(src, "managed()"), "1");
}

#[test]
fn labeled_block_returns_complete_only_the_target_block() {
	let src = r#"func block(): int = {
  let value = result@{ return@result 7 }
  value + 1
}
func nested(): int = {
  let value = outer@{ inner@{ return@outer 9 } 1 }
  value + 2
}
func callable_iife(): int = {
  let value = block@{ if (true) { return@callable_iife 13 } 3 }
  value + 4
}
func callable_iife_fallthrough(): int = {
  let value = block@{ if (false) { return@callable_iife_fallthrough 13 } 3 }
  value + 4
}
func direct_body(flag: boolean): int = result@{
  if (flag) { return@direct_body 17 }
  return@result 19
}"#;
	assert_eq!(run(src, "block()"), "8");
	assert_eq!(run(src, "nested()"), "11");
	assert_eq!(run(src, "callable_iife_fallthrough()"), "7");
	assert_eq!(run(src, "callable_iife()"), "13");
	assert_eq!(run(src, "direct_body(new NBool(true))"), "17");
	assert_eq!(run(src, "direct_body(new NBool(false))"), "19");
}

#[test]
fn receiverless_generic_calls_use_and_forward_canonical_type_objects() {
	let source = r#"
interface Seed { func seed(value: int): int }
impl Seed for int { func seed(value: int) = value + 1 }

struct Marker(value: int)
impl Seed for Marker { func seed(value: int) = value + 10 }

enum Token { Value }
impl Seed for Token { func seed(value: int) = value + 100 }

func direct<T: Seed>(marker: T, value: int): int = T.seed(value)
func forward<U: Seed>(marker: U, value: int): int = direct(marker, value)
func both<A: Seed, B: Seed>(a: A, b: B): int = A.seed(B.seed(1))
func answer(): int =
  forward(0, 40) + forward(Marker(value = 0), 1) +
  forward(Token.Value, 1) + both(0, Marker(value = 0))
"#;
	assert_eq!(run(source, "answer()"), "165");
}

#[test]
fn materialized_generic_default_uses_its_concrete_interface_type_object() {
	let source = r#"
interface Seed { func seed(): int }
impl Seed for int { func seed(): int = 41 }

interface Factory<T: Seed> { func make(): int = T.seed() }
struct Token()
impl Factory<int> for Token {}

func answer(): int = Token().make()
"#;
	assert_eq!(run(source, "answer()"), "41");
}

#[test]
fn receiverless_attachments_precede_top_level_initializers() {
	let source = r#"
interface Seed { func seed(value: int): int }
impl Seed for int { func seed(value: int): int = value + 1 }
func direct<T: Seed>(marker: T, value: int): int = T.seed(value)
let seeded = direct(0, 40)
func answer(): int = seeded
"#;
	assert_eq!(run(source, "answer()"), "41");
}

#[test]
fn grouped_generic_calls_forward_hidden_arguments() {
	let source = r#"
interface Seed { func seed(value: int): int }
impl Seed for int { func seed(value: int): int = value + 1 }
func direct<T: Seed>(marker: T, value: int): int = T.seed(value)
func answer(): int = (direct(0, 40))
"#;
	assert_eq!(run(source, "answer()"), "41");
}

#[test]
fn parameterized_nominals_share_canonical_receiverless_and_instance_dispatch() {
	let source = r#"
interface Seed { func seed(): int }

struct Box<T>(value: T)
impl Seed for Box<int> { func seed(): int = 1 }
impl Seed for Box<string> { func seed(): int = 2 }

enum Token<T> { Value(value: T), Empty }
impl Seed for Token<int> { func seed(): int = 3 }
impl Seed for Token<string> { func seed(): int = 4 }

func static_seed<T: Seed>(value: T): int = T.seed()
func instance_seed<T: Seed>(value: T): int = value.seed()
func empty<T>(): Token<T> = Token.Empty
func int_empty(): Token<int> = empty()
func string_empty(): Token<string> = empty()

func answer(): int = {
  let result = static_seed(Box(value = 0))
  let result = result + static_seed(Box(value = "")) * 10
  let result = result + instance_seed(Box(value = 0)) * 100
  let result = result + instance_seed(Box(value = "")) * 1000
  let result = result + static_seed(Token.Value(value = 0)) * 10000
  let result = result + instance_seed(Token.Value(value = "")) * 100000
  let result = result + instance_seed(int_empty()) * 1000000
  result + instance_seed(string_empty()) * 10000000
}
"#;
	assert_eq!(run(source, "answer()"), "43432121");
}

#[test]
fn incomplete_hidden_type_slots_do_not_erase_concrete_canonical_slots() {
	let source = r#"
interface Seed { func seed(value: int): int }
impl Seed for int { func seed(value: int) = value + 1 }

func direct<T: Seed, U>(marker: T, value: int): int = T.seed(value)
func answer(): int = direct(0, 40)
"#;
	let js = compile(source);
	assert!(js.contains("void 0"), "{js}");
	assert_eq!(run_js(js, "answer()"), "41");
}

#[test]
fn generic_construction_rejects_a_required_erased_hidden_argument() {
	let source = r#"
enum Token<T> { Empty }
func make<T>(): Token<T> = Token.Empty
func answer(): void = {
  make()
  return
}
"#;
	let diagnostics = nymph_compiler::compile(source, "test").expect_err("T is underdetermined");
	assert!(
		diagnostics.iter().any(|diagnostic| diagnostic
			.message
			.contains("erased runtime type argument required by receiverless dispatch")),
		"{diagnostics:?}"
	);
}

#[test]
fn nested_generic_construction_rejects_a_required_erased_hidden_argument() {
	let source = r#"
struct Box<T>(value: T)
enum Token<T> { Empty }
func make<T>(): Token<T> = Token.Empty
func outer<U>(): Token<Box<U>> = make()
func answer(): void = {
  outer()
  return
}
"#;
	let diagnostics = nymph_compiler::compile(source, "test").expect_err("U is underdetermined");
	assert!(
		diagnostics.iter().any(|diagnostic| diagnostic
			.message
			.contains("erased runtime type argument required by receiverless dispatch")),
		"{diagnostics:?}"
	);
}

#[test]
fn erased_hidden_slot_required_by_receiverless_dispatch_is_rejected() {
	let source = r#"
interface Seed { func seed(value: int): int }
impl Seed for int { func seed(value: int) = value + 1 }

func direct<T: Seed>(value: int): int = T.seed(value)
func answer(): void = {
  direct(40)
  return
}
"#;
	let diagnostics = nymph_compiler::compile(source, "test").expect_err("T is underdetermined");
	assert!(
		diagnostics.iter().any(|diagnostic| diagnostic
			.message
			.contains("erased runtime type argument required by receiverless dispatch")),
		"{diagnostics:?}"
	);
}

#[test]
fn transitively_required_erased_hidden_slot_is_rejected() {
	let source = r#"
interface Seed { func seed(value: int): int }
impl Seed for int { func seed(value: int) = value + 1 }

func inner<T: Seed>(value: int): int = T.seed(value)
func outer<U: Seed>(value: int): int = inner(value)
func answer(): void = {
  outer(40)
  return
}
"#;
	let diagnostics = nymph_compiler::compile(source, "test").expect_err("U is underdetermined");
	assert!(
		diagnostics.iter().any(|diagnostic| diagnostic
			.message
			.contains("erased runtime type argument required by receiverless dispatch")),
		"{diagnostics:?}"
	);
}

#[test]
fn receiverless_dispatch_demands_one_canonical_blanket_implementation() {
	let source = r#"
interface Seed { func seed(): int }
impl Seed for int { func seed(): int = 1 }

impl<T> Seed for T { func seed(): int = 2 }
struct Box<T>(value: T)

func answer(): int = 0.seed() + Box(value = 0).seed()
"#;
	let js = nymph_compiler::compile(source, "test").expect("blanket implementation lowers");
	assert!(
		js.contains("let $m0$impl$i1$seed = nymphCallable(function("),
		"blanket member must be one canonical top-level function: {js}"
	);
	assert!(
		!js.contains(
			"class {\n\tconstructor(fields) {\n\t\tObject.assign(this, fields);\n\t}\n\tseed()"
		)
	);
	assert_eq!(run(source, "answer()"), "3");
}

#[test]
fn blanket_body_forwards_implementation_arguments_before_nested_generic_arguments() {
	let source = r#"
interface Seed { func seed(): int }
impl Seed for int { func seed(): int = 1 }
impl Seed for string { func seed(): int = 10 }
impl Seed for boolean { func seed(): int = 100 }

interface Probe { func probe(): int }
func combine<T: Seed, U: Seed>(left: T, right: U): int = T.seed() + U.seed()
impl<T: Seed> Probe for T {
  func probe(): int = combine(this, true)
}

func answer(): int = "".probe()
"#;
	let js = nymph_compiler::compile(source, "test").expect("generic implementation lowers");
	assert!(
		js.contains("= combine;")
			&& js.contains("new NBool(true)")
			&& js.contains("NBool.prototype")
			&& js.contains("return nymphTailCall("),
		"nested call must carry its source arguments before implementation and nested generic runtime objects: {js}"
	);
	assert_eq!(run(source, "answer()"), "110");
}

#[test]
fn blanket_membership_preserves_source_argument_and_hidden_slot_order() {
	let source = r#"
interface Seed { func seed(): int }
impl Seed for int { func seed(): int = 7 }
interface Contains<Item> { func contains(item: Item): boolean }
impl<T: Seed> Contains<string> for T {
  func contains(item: string): boolean = T.seed() == 7
}
func answer(): boolean = "" in 7
"#;
	let js = nymph_compiler::compile(source, "test").expect("blanket membership lowers");
	assert!(
		js.contains("= $m0$impl$i1$contains;")
			&& js.contains("new NInt(7n)")
			&& js.contains("NInt.prototype")
			&& js.contains("return nymphTailCall("),
		"membership source argument must precede the preserved implementation hidden slot: {js}"
	);
	assert_eq!(run(source, "answer()"), "true");
}

#[test]
fn blanket_materialized_default_forwards_implementation_arguments_to_override() {
	let source = r#"
interface Seed { func seed(): int }
impl Seed for int { func seed(): int = 1 }

interface Probe {
  func target(): int
  func probe(): int = this.target()
}
impl<T: Seed> Probe for T {
  func target(): int = T.seed()
}

func answer(): int = 0.probe()
"#;
	assert_eq!(run(source, "answer()"), "1");
}

#[test]
fn first_class_blanket_method_preserves_source_before_hidden_abi() {
	let source = r#"
interface Seed { func seed(value: int): int }
impl<T> Seed for T { func seed(value: int): int = value }

func answer(): int = {
  let seed = 0.seed
  seed(7)
}
"#;
	let js = nymph_compiler::compile(source, "test").expect("blanket method value lowers");
	assert!(
		js.contains("nymphPush(") && !js.contains("nymphActivate($m0$impl$i0$seed"),
		"method-value adapter must transfer through the activation driver: {js}"
	);
	assert_eq!(run(source, "answer()"), "7");
}

#[test]
fn first_class_generic_blanket_methods_capture_method_hidden_arguments() {
	let source = r#"
interface Seed { func seed(value: int): int }
impl Seed for string { func seed(value: int): int = value + 40 }

interface Apply {
  func apply<T: Seed>(marker: T, value: int): int = T.seed(value)
}
impl<U> Apply for U {}

func stored(): int = {
  let apply = 0.apply
  apply("", 1)
}
func grouped(): int = (0.apply)("", 2)
func immediate(): int = 0.apply("", 3)
"#;
	let js = nymph_compiler::compile(source, "test").expect("generic blanket method values lower");
	assert!(
		js.contains("nymphPush(") && !js.contains("nymphActivate($m0$impl$i1$apply"),
		"generic method-value adapters must transfer through the activation driver: {js}"
	);
	assert_eq!(
		run(source, "stored().v + grouped().v + immediate().v"),
		"126"
	);
}

#[test]
fn generic_bound_method_forwards_its_hidden_arguments() {
	let source = r#"
interface Seed { func seed(value: int): int }
impl Seed for int { func seed(value: int) = value + 1 }

interface Apply {
  func apply<T: Seed>(marker: T, value: int): int = T.seed(value)
}
impl Apply for int {}

func invoke<A: Apply, T: Seed>(apply: A, marker: T, value: int): int =
  apply.apply(marker, value)
func answer(): int = invoke(0, 0, 40)
"#;
	assert_eq!(run(source, "answer()"), "41");
}

#[test]
fn generic_dispatched_method_rejects_a_required_erased_hidden_argument() {
	let source = r#"
interface Seed { func seed(value: int): int }
impl Seed for int { func seed(value: int) = value + 1 }

interface Apply {
  func apply<T: Seed>(value: int): int = T.seed(value)
}
impl Apply for int {}

func invoke<A: Apply>(apply: A): int = apply.apply(40)
func answer(): int = invoke(0)
"#;
	let diagnostics = nymph_compiler::compile(source, "test").expect_err("T is underdetermined");
	assert!(
		diagnostics.iter().any(|diagnostic| diagnostic
			.message
			.contains("erased runtime type argument required by receiverless dispatch")),
		"{diagnostics:?}"
	);
}

#[test]
fn grouped_generic_dispatch_rejects_a_required_erased_hidden_argument() {
	let source = r#"
interface Seed { func seed(value: int): int }
impl Seed for int { func seed(value: int) = value + 1 }

interface Apply {
  func apply<T: Seed>(value: int): int = T.seed(value)
}
impl Apply for int {}

func invoke<A: Apply>(apply: A): int = (apply.apply(40))
func answer(): int = invoke(0)
"#;
	let diagnostics = nymph_compiler::compile(source, "test").expect_err("T is underdetermined");
	assert!(
		diagnostics.iter().any(|diagnostic| diagnostic
			.message
			.contains("erased runtime type argument required by receiverless dispatch")),
		"{diagnostics:?}"
	);
}

#[test]
fn higher_order_generic_calls_keep_hidden_arguments_on_the_factory_call() {
	let source = r#"
interface Seed { func seed(value: int): int }
impl Seed for int { func seed(value: int) = value + 1 }

func factory<T: Seed>(marker: T): () -> int = () -> T.seed(40)
func answer(): int = (factory(0))()
"#;
	assert_eq!(run(source, "answer()"), "41");
}

#[test]
fn generic_callable_values_capture_hidden_type_objects() {
	let source = r#"
interface Seed { func seed(value: int): int }
impl Seed for int { func seed(value: int) = value + 1 }

func direct<T: Seed>(marker: T, value: int): int = T.seed(value)
func invoke(callback: (int, int) -> int): int = callback(0, 40)
func through_alias(): int = {
  let alias = direct
  alias(0, 40)
}
func through_callback(): int = invoke(direct)
func recursive<T: Seed>(marker: T, value: int): int = {
  let again = recursive
  if (value == 0) { T.seed(40) } else { again(marker, value - 1) }
}
func answer(): int = through_alias() + through_callback() + recursive(0, 1)
"#;
	assert_eq!(run(source, "answer()"), "123");
}

#[test]
fn generic_pipeline_calls_forward_one_hidden_argument() {
	let source = r#"
interface Seed { func seed(value: int): int }
impl Seed for int { func seed(value: int) = value + 1 }

func apply<T: Seed>(marker: T): int = T.seed(40)
func answer(): int = 0 |> apply
"#;
	assert_eq!(run(source, "answer()"), "41");
}

#[test]
fn enum_views_erase_without_wrappers_and_dispatch_statically() {
	let source = r#"
enum Source {
	A,
	Value(value: int),
  func value(): int = 1
}
enum View {
  ...Source,
  B
  func value(): int = 2
}
func viewed(value: View): int = value.value()
func identity(): View = Source.A
func direct(): int = Source.A.value()
func indirect(): int = viewed(Source.A)
func answer(): int = direct() + indirect()
"#;
	assert_eq!(run(source, "identity() === Source.A"), "true");
	assert_eq!(run(source, "direct()"), "1");
	assert_eq!(run(source, "indirect()"), "2");
	assert_eq!(run(source, "answer()"), "3");
	let js = compile(source);
	assert!(
		!js.contains("View.A"),
		"destination enum must not emit a source variant alias: {js}"
	);
}

#[test]
fn question_uses_an_explicit_into_fallback_for_result_errors() {
	let source = r#"
struct Narrow(code: int)
struct Wide(code: int)
impl Into<Wide> for Narrow {
  func into(): Wide = Wide(code = this.code + 1)
}
func inner(): Result<int, Narrow> = Error(Narrow(code = 4))
func outer(): Result<int, Wide> = Ok(inner()?)
"#;
	assert_eq!(run(source, "outer().error.code.v"), "5");
}
