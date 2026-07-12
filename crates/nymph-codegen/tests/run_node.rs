//! End-to-end: parse -> check -> lower -> emit -> run under Node, asserting stdout.

use std::io::Write;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use nymph_codegen::emit;
use nymph_sema::{check_module, lower_hir};
use nymph_syntax::parse_module;

/// Compile a Nymph source module to a JS module string.
fn compile(src: &str) -> String {
	let parsed = parse_module(src, "test");
	assert!(
		!parsed.diagnostics.iter().any(|d| d.is_error()),
		"parse errors in test source: {:?}",
		parsed.diagnostics
	);
	let checked = check_module(&parsed.tree);
	assert!(
		checked.diags.is_empty(),
		"check errors: {:?}",
		checked.diags
	);
	emit(&lower_hir(&parsed.tree, &checked))
}

/// Emit `src`, append a driver that logs `expr`, run under Node, return trimmed stdout.
fn run(src: &str, call: &str) -> String {
	let mut js = compile(src);
	js.push_str(&format!("\nconsole.log({call});\n"));

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

#[test]
fn runs_arithmetic() {
	// Pure scalar arithmetic (Task 3/4 already cover emit+lower; this asserts it RUNS).
	let out = run("func add(a: int, b: int): int = a + b * 2", "add(3, 4)");
	assert_eq!(out, "11");
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
	assert_eq!(run(src, "sign(5)"), "1");
	assert_eq!(run(src, "sign(-3)"), "-1");
	assert_eq!(run(src, "sign(0)"), "0");
}

#[test]
fn runs_while_loop() {
	// A `while` loop with a mutable accumulator; assignment (`=`) drives it.
	let src = r#"
		func sum_to(n: int): int = {
			let mut total = 0
			let mut i = 1
			while (i <= n) {
				total = total + i
				i = i + 1
			}
			total
		}
	"#;
	assert_eq!(run(src, "sum_to(5)"), "15");
}

#[test]
fn runs_list_and_index() {
	// A list literal emits as a JS array; indexing is a computed member `arr[i]`.
	let src = "func third(): int = #[10, 20, 30][2]";
	assert_eq!(run(src, "third()"), "30");
}

#[test]
fn runs_tuple_roundtrip() {
	// A tuple emits as a JS array — `JSON.stringify` proves the shape survives.
	let src = "func pair(): #(int, int) = #(1, 2)";
	assert_eq!(run(src, "JSON.stringify(pair())"), "[1,2]");
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
	assert_eq!(run(src, "sum(new Point({ x: 10, y: 20 }))"), "30");
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
	assert_eq!(run(src, "classify(0)"), "100");
	assert_eq!(run(src, "classify(1)"), "200");
	assert_eq!(run(src, "classify(9)"), "300");
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
	assert_eq!(run(src, "f([0, 7])"), "7"); // first arm (literal 0 matches)
	assert_eq!(run(src, "f([20, 1])"), "20"); // guard passes
	assert_eq!(run(src, "f([5, 1])"), "0"); // guard fails → fall through
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
	assert_eq!(run(src, "f(new Point({ x: 3, y: 4 }))"), "7");
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
	assert_eq!(run(src, "f(Opt.Some({ value: 41 }))"), "42");
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
	assert_eq!(run(src, "head_or([])"), "-1"); // exact-length #[] arm
	assert_eq!(run(src, "head_or([7, 8, 9])"), "7"); // spread arm binds head
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
	assert_eq!(run(src, "ends([10, 2, 3, 20])"), "30"); // a=10, b=20 (mid=[2,3])
	assert_eq!(run(src, "ends([1, 9])"), "10"); // a=1, b=9, mid=[]
	assert_eq!(run(src, "ends([5])"), "-1"); // length 1 < 2 → wildcard
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
	assert_eq!(run(src, "lookup(new Map([[1, 42]]))"), "42");
	assert_eq!(run(src, "lookup(new Map([[2, 9]]))"), "-1");
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
	assert_eq!(run(n, "size(5)"), "1");
	assert_eq!(run(n, "size(100)"), "2");
	assert_eq!(run(n, "size(500)"), "3");
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
fn runs_struct_method_with_this() {
	// An inherent method emits as a class method; `this` reads the instance's fields.
	let src = r#"
		struct Point(x: int, y: int)
		impl Point {
			func sum(): int = this.x + this.y
		}
		func total(p: Point): int = p.sum()
	"#;
	assert_eq!(run(src, "total(new Point({ x: 3, y: 4 }))"), "7");
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
	assert_eq!(run(src, "bump(new Counter({ n: 5 }))"), "15");
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
	assert_eq!(run(src, "new Counter({ n: 21 }).doubled()"), "42");
}

#[test]
fn runs_struct_inner_func() {
	// A method declared inside the struct body itself (not a top-level `impl` block).
	let src = r#"
		struct Point(x: int, y: int) {
			func sum(): int = this.x + this.y
		}
	"#;
	assert_eq!(run(src, "new Point({ x: 10, y: 5 }).sum()"), "15");
}

#[test]
fn compile_reports_check_errors() {
	// A type error surfaces as diagnostics, not JS.
	let result = nymph_codegen::compile("func f(): int = true", "test");
	assert!(result.is_err(), "type error should not produce JS");
}

#[test]
fn compile_produces_runnable_js() {
	let result = nymph_codegen::compile("func double(n: int): int = n * 2", "test");
	assert!(
		result.is_ok(),
		"well-typed program should compile: {result:?}"
	);
}
