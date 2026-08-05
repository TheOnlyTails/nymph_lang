//! End-to-end: parse -> check -> lower -> emit -> run under Node, asserting stdout.

use std::io::Write;
use std::process::Command;
use std::sync::{
	OnceLock,
	atomic::{AtomicU64, Ordering},
};

use nymph_codegen::emit;
use nymph_sema::{check_module_with_prelude, lower_hir_with_prelude};
use nymph_syntax::parse_module;

/// Compile a Nymph source module to a JS module string.
fn compile(src: &str) -> String {
	nymph_compiler::compile(src, "test")
		.unwrap_or_else(|diagnostics| panic!("compile errors: {diagnostics:?}"))
}

/// Compile `user_src` against `prelude_src` (a single prelude module, checked and
/// lowered the same paired way `check_module_with_prelude`/`lower_hir_with_prelude`
/// are documented to be used — see `crates/nymph-sema/tests/prelude.rs`) to a JS
/// module string. Used by [`run_with_prelude`] to drive the
/// collections-materialization payoff (and negative-defer) tests under Node,
/// mirroring [`compile`] but threading a prelude module through.
fn compile_with_prelude(user_src: &str, prelude_src: &str) -> String {
	let user = parse_module(user_src, "test");
	assert!(
		!user.diagnostics.iter().any(|d| d.is_error()),
		"parse errors in user source: {:?}",
		user.diagnostics
	);
	let prelude = parse_module(prelude_src, "prelude");
	assert!(
		!prelude.diagnostics.iter().any(|d| d.is_error()),
		"parse errors in prelude source: {:?}",
		prelude.diagnostics
	);
	let prelude_modules = std::slice::from_ref(&prelude.tree);
	let checked = check_module_with_prelude(&user.tree, prelude_modules);
	assert!(
		checked.diags.iter().all(|d| !d.is_error()),
		"check errors: {:?}",
		checked.diags
	);
	emit(&lower_hir_with_prelude(
		&user.tree,
		prelude_modules,
		&checked,
	))
}

/// Append a driver that logs `call`, run the already-compiled `js` module under
/// Node, and return trimmed stdout. Shared by [`run`] and [`run_with_prelude`].
fn run_js(mut js: String, call: &str) -> String {
	// Nymph values are boxed at the generated-JS boundary. Most tests in this
	// file assert a program's observable scalar result rather than its runtime
	// representation, so unwrap boxes before handing the value to `console.log`.
	// Tests that deliberately inspect representation still do so inside `call`
	// (for example with `JSON.stringify` or `[TAG]`) and therefore pass a raw JS
	// scalar/string through this helper unchanged.
	js.push_str(&format!(
		"\nfunction nymphTestValue(value) {{\n\
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

/// Same as [`run`], but `user_src` is checked/lowered against `prelude_src` as a
/// prelude module (`check_module_with_prelude`/`lower_hir_with_prelude`) — for
/// driving the collections-materialization payoff under Node.
fn run_with_prelude(user_src: &str, prelude_src: &str, call: &str) -> String {
	run_js(compile_with_prelude(user_src, prelude_src), call)
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
	assert_eq!(run(src, "sum_to(new NInt(5))"), "15");
}

#[test]
fn runs_list_and_index() {
	// A list literal emits as a JS array; indexing is a computed member `arr[i]`.
	let src = "func third(): int = #[10, 20, 30][2]";
	assert_eq!(run(src, "third()"), "30");
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
	// `#{ ...rest }` with no named entries — rest is the whole map, copied.
	let src = r#"
		func copy(m: #{int: int}): #{int: int} = match (m) {
			#{ ...rest } -> rest,
			_ -> m,
		}
	"#;
	assert_eq!(
		run(src, "JSON.stringify([...copy(new Map([[1, 1]]))])"),
		"[[1,1]]"
	);
	// The result is a distinct Map object, not an alias of the input.
	let mutation_check = r#"
		const original = new Map([[1, 1]]);
		const result = copy(original);
		result.set(2, 2);
		return JSON.stringify([[...original], [...result]]);
	"#;
	assert_eq!(
		run(src, &format!("(() => {{ {mutation_check} }})()")),
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
fn bound_union_exposes_the_binding_and_evaluates_the_scrutinee_once() {
	let src = r#"
		struct Counter(n: int) {}
		impl Counter {
			mut func next(): int = {
				this.n = this.n + 1
				2
			}
		}
		func capture(): #(int, int) = {
			let mut counter = Counter(n = 0)
			let selected = match (counter.next()) {
				(x = 1 | x = 2) -> x,
				_ -> 0,
			}
			#(selected, counter.n as int)
		}
	"#;
	assert_eq!(run(src, "capture()"), "[ 2, 1 ]");
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
fn runs_compound_assign_dispatches_user_operator() {
	// `v1 += v2` on a struct with a directly-defined `Plus.plus` impl actually calls
	// `.plus(...)` at runtime, rather than emitting a literal JS `v1 = v1 + v2`
	// (which would silently string-coerce two class instances) — Finding 1.
	let src = r#"
		interface Plus<Other, Output> { func plus(other: Other): Output }
		struct Vec2(x: int, y: int)
		impl Plus<Other = Vec2, Output = Vec2> for Vec2 {
			func plus(other: Vec2): Vec2 = Vec2(x = this.x + other.x, y = this.y + other.y)
		}
		func combine(a: Vec2, b: Vec2): Vec2 = {
			let mut v1 = a
			v1 += b
			v1
		}
	"#;
	assert_eq!(
		run(
			src,
			"combine(new Vec2({ x: new NInt(1), y: new NInt(2) }), new Vec2({ x: new NInt(3), y: new NInt(4) })).x"
		),
		"4"
	);
	assert_eq!(
		run(
			src,
			"combine(new Vec2({ x: new NInt(1), y: new NInt(2) }), new Vec2({ x: new NInt(3), y: new NInt(4) })).y"
		),
		"6"
	);
}

#[test]
fn runs_compound_assign_on_int_stays_native() {
	// `x += 1` on a plain `int` still runs as a native JS `+=`, not a dispatched call.
	let src = r#"
		func bump(): int = {
			let mut x = 10
			x += 5
			x
		}
	"#;
	assert_eq!(run(src, "bump()"), "15");
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
fn user_struct_operators_use_identity_while_explicit_equals_dispatches() {
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
		"false"
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
		"true"
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
fn runs_return_inside_a_statement_position_while() {
	// A `return` inside a `while` body must target the enclosing function, not
	// some IIFE — the `while` body is flattened via `block_stmt`, never wrapped.
	// (`result` starts with `-1` on the SAME statement as its `let`, not as a
	// line-leading operator that would continue the `while` via subtraction.)
	let src = r#"
		func first_over(xs: #[int], limit: int): int = {
			let mut i = 0
			let mut result = -1
			while (i < 3) {
				if (xs[i] > limit) { return xs[i] }
				i += 1
			}
			result
		}
	"#;
	assert_eq!(
		run(
			src,
			"first_over(new NList([new NInt(1), new NInt(5), new NInt(9)]), new NInt(3))"
		),
		"5"
	);
	assert_eq!(
		run(
			src,
			"first_over(new NList([new NInt(1), new NInt(2), new NInt(3)]), new NInt(100))"
		),
		"-1"
	);
}

#[test]
fn runs_return_inside_a_statement_position_match() {
	// A braced match-arm body (`-> { return .. }`) in a STATEMENT-position match
	// (not a subexpression) emits for free — the whole `match` is flattened via
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
fn return_works_across_general_expression_positions_and_preserves_evaluation() {
	let src = r#"
func id(value: int): int = value
func direct_operand(): int = 1 + return 25
func operand(): int = 1 + if (true) return 2 else 0
func lazy_and(): int = { false && return 3
4 }
func lazy_or(): int = { true || return 5
6 }
func lazy_taken(): int = { true && return 26
0 }
func prefix_operand(): int = -(return 27)
func cast_operand(): int = (return 28) as int
func compound_operand(): int = { let mut value = 1
value += return 29
value }
func anonymous_boundary(): int = ({ return 30
$0 })(1)
func callee(): int = (return 7)()
func argument(): int = id(return 8)
func member(): int = (return 9).field
func index(): int = #[1][return 10]
func list_element(): int = #[1, return 11][0]
func list_spread(): int = #[1, ...(if (true) return 12 else #[2])][0]
func map_element(): int = #{ 1: return 13 }[1]
func map_spread(): int = { #{ 1: 1, ...(if (true) return 14 else #{ 2: 2 }) }
0 }
func tuple_element(): int = { #(1, return 15)
0 }
func tuple_spread(): int = { #(1, ...(if (true) return 16 else #(2)))
0 }
func interpolation(): int = { "value=${return 14}"
0 }
func nested_block(): int = 1 + { 2 + { if (true) return 15 else 0 } }
func condition(): int = { while (return 16) {} 0 }
func loop_body(): int = { while (true) return 17
0 }
func for_body(): int = { for (value in #[24]) return value
0 }
func arm(flag: boolean): int = if (flag) return 18 else match (0) { 0 -> return 19, _ -> 0 }

func ordered(flag: boolean): int = {
	let mut seen = 0
	let mark = (value: int) -> { seen = seen * 10 + value
	value }
	let result = id(mark(1)) + if (flag) return seen * 10 + 2 else mark(2)
	result * 10 + seen
}

func pipe_order(flag: boolean): int = {
	let mut seen = 0
	let mark = (value: int) -> { seen = seen * 10 + value
	value }
	let id = (value: int) -> value
	let result = mark(1) |> (if (flag) return seen else id)
	result * 10 + seen
}

func membership_order(flag: boolean): int = {
	let mut seen = 0
	let mark = (value: int) -> { seen = seen * 10 + value
	value }
	let found = mark(1) in (if (flag) return seen else #[1])
	if (found) seen else 0
}

func loop_completion(flag: boolean): int = {
	let result = while (true) {
		if (flag) { return 20 }
		false && continue
		break 3
	}
	match (result) { Some(value) -> value, None -> 0 }
}

func closure_boundary(): int = {
	let inner = (value: int) -> id(return value)
	inner(21) + 1
}

struct Value(value: int) {
	func method(flag: boolean): int = this.value + if (flag) return 22 else 1
}

interface DefaultValue {
	func default_value(flag: boolean): int = 1 + if (flag) return 23 else 2
}
impl DefaultValue for Value {}
"#;
	for (call, expected) in [
		("direct_operand()", "25"),
		("operand()", "2"),
		("lazy_and()", "4"),
		("lazy_or()", "6"),
		("lazy_taken()", "26"),
		("prefix_operand()", "27"),
		("cast_operand()", "28"),
		("compound_operand()", "29"),
		("anonymous_boundary()", "30"),
		("callee()", "7"),
		("argument()", "8"),
		("member()", "9"),
		("index()", "10"),
		("list_element()", "11"),
		("list_spread()", "12"),
		("map_element()", "13"),
		("map_spread()", "14"),
		("tuple_element()", "15"),
		("tuple_spread()", "16"),
		("interpolation()", "14"),
		("nested_block()", "15"),
		("condition()", "16"),
		("loop_body()", "17"),
		("for_body()", "24"),
		("arm(new NBool(true))", "18"),
		("arm(new NBool(false))", "19"),
		("ordered(new NBool(true))", "12"),
		("ordered(new NBool(false))", "42"),
		("pipe_order(new NBool(true))", "1"),
		("pipe_order(new NBool(false))", "11"),
		("membership_order(new NBool(true))", "1"),
		("membership_order(new NBool(false))", "1"),
		("loop_completion(new NBool(true))", "20"),
		("loop_completion(new NBool(false))", "3"),
		("closure_boundary()", "22"),
		(
			"new Value({ value: new NInt(30) }).method(new NBool(true))",
			"22",
		),
		(
			"new Value({ value: new NInt(30) }).method(new NBool(false))",
			"31",
		),
		(
			"new Value({ value: new NInt(30) }).default_value(new NBool(true))",
			"23",
		),
		(
			"new Value({ value: new NInt(30) }).default_value(new NBool(false))",
			"3",
		),
	] {
		assert_eq!(run(src, call), expected, "{call}");
	}
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
fn runs_mutable_top_level_let() {
	let src = "let mut counter = 0
	           func bump(): int = counter + 1";
	assert_eq!(run(src, "bump()"), "1");
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
fn unresolved_callable_value_initializer_calls_are_compile_errors() {
	let src = "let callback = () -> later\nlet result = callback()\nlet later = 5";
	let result = nymph_compiler::compile(src, "test");
	let diagnostics = result.expect_err("dynamic initializer call must not produce JavaScript");
	assert!(
		diagnostics
			.iter()
			.any(|diagnostic| diagnostic.message.contains("UnresolvedInitializerCall")),
		"{diagnostics:?}"
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
fn runs_bare_return_as_an_unbraced_while_body() {
	let src = r#"
		func f(n: int): int = {
			while (n > 0) return n
			0
		}
	"#;
	assert_eq!(run(src, "f(new NInt(5))"), "5");
	assert_eq!(run(src, "f(new NInt(0))"), "0");
}

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
fn runs_string_compound_assign() {
	let src = r#"
		func f(): string = {
			let mut s = "a"
			s += "b"
			s
		}
	"#;
	assert_eq!(run(src, "f()"), "ab");
}

// ── Slice 4H: range/for-loop expressions ────────────────────────────────────

#[test]
fn runs_a_for_loop_over_an_exclusive_range() {
	let src = r#"
		func sum_to(n: int): int = {
			let mut total = 0
			for (i in 1..n) {
				total = total + i
			}
			total
		}
	"#;
	// 1..5 exclusive: 1 + 2 + 3 + 4 = 10.
	assert_eq!(run(src, "sum_to(new NInt(5))"), "10");
}

#[test]
fn runs_a_for_loop_over_an_inclusive_range() {
	let src = r#"
		func sum_to_inclusive(n: int): int = {
			let mut total = 0
			for (i in 1..=n) {
				total = total + i
			}
			total
		}
	"#;
	// 1..=5 inclusive: 1 + 2 + 3 + 4 + 5 = 15.
	assert_eq!(run(src, "sum_to_inclusive(new NInt(5))"), "15");
}

#[test]
fn runs_integer_range_protocol_edges() {
	let src = r#"
		func uint_sum(): uint = {
			let mut total = 0u
			for (i in 1u..4u) { total = total + i }
			total
		}
		func equal_exclusive(): int = {
			let mut count = 0
			for (_ in 2..2) { count = count + 1 }
			count
		}
		func equal_inclusive(): int = {
			let mut count = 0
			for (_ in 2..=2) { count = count + 1 }
			count
		}
		func descending(): int = {
			let mut count = 0
			for (_ in 3..1) { count = count + 1 }
			count
		}
	"#;
	assert_eq!(
		run(
			src,
			"[uint_sum().v, equal_exclusive().v, equal_inclusive().v, descending().v].join(',')"
		),
		"6,0,1,0"
	);
}

#[test]
fn runs_a_for_loop_with_a_call_expression_upper_bound() {
	// The upper bound is hoisted into a temp and evaluated once, up front — this
	// just pins that a call-expression bound lowers and runs correctly at all.
	let src = r#"
		func upper(): int = 4
		func sum_to_call_bound(): int = {
			let mut total = 0
			for (i in 1..upper()) {
				total = total + i
			}
			total
		}
	"#;
	// 1..4 exclusive: 1 + 2 + 3 = 6.
	assert_eq!(run(src, "sum_to_call_bound().v"), "6");
}

#[test]
fn runs_a_for_loop_with_a_parenthesized_range_bound() {
	// A parenthesized range bound is `ExprKind::Grouped` in the AST, which the
	// checker's `check()` recurses through without recording an annotation for
	// the `Grouped` node's own id. Lowering must peel through the parens to
	// find the real numeric-element annotation rather than panicking on a
	// perfectly valid program. Covers both a parenthesized literal bound and a
	// parenthesized binary-expression bound.
	let src = r#"
		func sum_paren_literal(): int = {
			let mut total = 0
			for (i in (1)..5) {
				total = total + i
			}
			total
		}
		func sum_paren_binary(a: int, b: int, n: int): int = {
			let mut total = 0
			for (i in (a + b)..n) {
				total = total + i
			}
			total
		}
	"#;
	// 1..5 exclusive: 1 + 2 + 3 + 4 = 10.
	assert_eq!(run(src, "sum_paren_literal().v"), "10");
	// (1 + 2)..7 exclusive == 3..7: 3 + 4 + 5 + 6 = 18.
	assert_eq!(
		run(
			src,
			"sum_paren_binary(new NInt(1), new NInt(2), new NInt(7)).v",
		),
		"18"
	);
}

#[test]
fn range_expressions_are_canonical_values_and_evaluate_bounds_once_in_order() {
	let src = r#"
func pass(value: Range<int>): int = value.start * 10 + value.end
func returned(): RangeInclusive<int> = 3..=4
func exercise(): int = {
  let mut order = 0
  let endpoint = (value: int) -> { order = order * 10 + value value }
  let stored = endpoint(1)..endpoint(2)
  let from = (endpoint(3))..
  let to = ..endpoint(4)
  let inclusive = endpoint(5)..=endpoint(6)
  let to_inclusive = ..=endpoint(7)
  order * 10000000
    + pass(stored) * 100000
    + from.start * 10000
    + to.end * 1000
    + inclusive.start * 100
    + returned().end * 10
    + to_inclusive.end
}
"#;
	assert_eq!(run(src, "exercise()"), "12345671234547");
}

#[test]
fn runs_fallible_steps_and_directional_canonical_ranges() {
	let src = r#"
func push_digit(acc: int, value: int): int = acc * 10 + value

func direct_and_stored(): int = {
  let mut direct = 0
  for (value in 1..=3) { direct = push_digit(direct, value) }
  let stored = 1..=3
  let mut indirect = 0
  for (value in stored) { indirect = push_digit(indirect, value) }
  direct * 1000 + indirect
}

func reversed_ranges(): int = {
  let mut result = 0
  for (value in (1..4).reversed()) { result = push_digit(result, value) }
  for (value in (1..=4).reversed()) { result = push_digit(result, value) }
  for (value in (4..1).reversed()) { result = push_digit(result, value) }
  result
}

func reversed_startless(): int = {
  for (value in (..4).reversed()) {
    return value * 10 + inclusive_start()
  }
  0
}

func inclusive_start(): int = {
  for (value in (..=3).reversed()) {
    return value
  }
  0
}

func open_forward(): int = {
  for (value in 7..) {
    return value
  }
  0
}

func char_points(): int = {
  let mut result = 0
  for (value in '\uD7FE'..='\uE001') {
    let point = value as int
    result = result * 10 + if (point == 55294) 1 else if (point == 55295) 2 else if (point == 57344) 3 else 4
  }
  result
}

func first_class_next<T: Step>(value: T): Option<T> = {
  let next = value.next
  next()
}

func step_edges(): #(boolean, boolean, boolean, boolean, boolean, boolean, boolean, boolean, int, int) = #(
  9007199254740991.next().is_none(),
  (-9007199254740991).previous().is_none(),
  0u.previous().is_none(),
  (1114111 as char).next().is_none(),
  (0 as char).previous().is_none(),
  (-9007199254740992).next().is_none(),
  9007199254740992u.previous().is_none(),
  first_class_next(9007199254740991).is_none(),
  match ('\uD7FF'.next()) { Some(value) -> value as int, None -> -1 },
  match ('\uE000'.previous()) { Some(value) -> value as int, None -> -1 },
)

func direct_and_stored_boundary(): int = {
  let mut direct = 0
  for (_ in 9007199254740991..9007199254740994) { direct = direct + 1 }
  let stored = 9007199254740991..9007199254740994
  let mut indirect = 0
  for (_ in stored) { indirect = indirect + 1 }
  direct * 10 + indirect
}
"#;
	assert_eq!(run(src, "direct_and_stored()"), "123123");
	assert_eq!(run(src, "reversed_ranges()"), "3214321");
	assert_eq!(run(src, "reversed_startless()"), "33");
	assert_eq!(run(src, "open_forward()"), "7");
	assert_eq!(run(src, "char_points()"), "1234");
	assert_eq!(run(src, "direct_and_stored_boundary()"), "11");
	assert_eq!(
		run(src, "JSON.stringify(nymphTestValue(step_edges()))"),
		"[true,true,true,true,true,true,true,true,57344,55295]"
	);
}

#[test]
fn break_terminates_open_direct_and_stored_ranges() {
	let src = r#"
func exercise(): int = {
  let mut result = 0
  for (value in 7..) {
    result = value
    break
  }
  result
}

func bounded_direct(): int = {
  let mut result = 0
  for (value in 1..=5) {
    result = result * 10 + value
    if (value == 3) { break }
  }
  result
}

func bounded_stored(): int = {
  let range = 1..=5
  let mut result = 0
  for (value in range) {
    result = result * 10 + value
    if (value == 3) { break }
  }
  result
}
"#;
	assert_eq!(run(src, "exercise()"), "7");
	assert_eq!(run(src, "bounded_direct()"), "123");
	assert_eq!(run(src, "bounded_stored()"), "123");
	for diagnostics in [
		nymph_compiler::compile("func invalid(): void = { break }", "invalid").unwrap_err(),
		nymph_compiler::compile(
			"func invalid(): void = for (_ in 1..) { let stop = () -> { break } stop() }",
			"invalid_closure",
		)
		.unwrap_err(),
	] {
		assert!(
			diagnostics.iter().any(
				|diagnostic| diagnostic.message.contains("break") && diagnostic.message.contains("loop")
			),
			"{diagnostics:?}"
		);
	}
}

// ── Iterator for-loops (Tier 1, Track A) ─────────────────────────────────────

#[test]
fn runs_a_for_loop_over_a_list() {
	// Boxed lists route through `Iterable.iter()` / `Iterator.next()` like every
	// other collection; the source-only facade remains prelude-free here.
	let src = r#"
		func sum_list(): int = {
			let mut total = 0
			for (x in #[1, 2, 3, 4]) {
				total = total + x
			}
			total
		}
	"#;
	assert_eq!(run(src, "sum_list().v"), "10");
}

#[test]
fn runs_a_for_loop_over_a_mut_list() {
	// `mut` is transparent to the same uniform iteration protocol.
	let src = r#"
		func sum_mut_list(): int = {
			let mut xs = #[1, 2, 3, 4]
			let mut total = 0
			for (x in xs) {
				total = total + x
			}
			total
		}
	"#;
	assert_eq!(run(src, "sum_mut_list().v"), "10");
}

#[test]
fn runs_a_for_loop_over_an_iterator_directly() {
	// RR1/RR2: a source that itself implements `Iterator<Item>` is used
	// directly (`let $it = <src>`, no `.iter()` hop) — desugars to
	// `while ($go) { match ($it.next()) { Some(x) -> .., None -> $go = false } }`.
	// `Counter`'s `next` mutates `this.n`, which requires `this: mut Self` —
	// declaring `next` a `mut func` on BOTH the interface (OO1, the source of
	// truth every gate reads) and this impl (OO2, the impl's restatement must
	// match) gets that without needing a `Mut`-self-type impl target (`impl ..
	// for mut Counter`), which lowering doesn't support yet for a class-backed
	// ADT (a separate, pre-existing MT2 lowering gap this slice doesn't touch).
	// `c` itself must be bound `mut`: the `for`-loop desugar calls `next()` on
	// it directly (`IterMode::Direct`), and that call is gated exactly like an
	// explicit `c.next()` would be (`MutMethodNeedsMutReceiver`).
	let src = r#"
		struct Counter(n: int, max: int)
		impl Iterator<int> for Counter {
			mut func next(): Option<int> = if (this.n > this.max) {
				None
			} else {
				let v = this.n
				this.n = this.n + 1
				Some(value = v)
			}
		}
		func sum_counter(): int = {
			let mut c = Counter(n = 1, max = 4)
			let mut total = 0
			for (x in c) {
				total = total + x
			}
			total
		}
	"#;
	// 1 + 2 + 3 + 4 = 10.
	assert_eq!(run(src, "sum_counter()"), "10");
}

#[test]
fn runs_a_for_loop_over_an_iterable_via_iter() {
	// RR1/RR2: a source that implements `Iterable<T>` (not `Iterator` itself)
	// is desugared through `.iter()` — `let $it = <src>.iter()` — then the
	// same while/match/next protocol as the direct case above. `T` is read off
	// the matched `Iterable` impl's own argument (`resolve_iface_arg`), not by
	// typing `iter()`'s return — the checker doesn't cross-check an impl
	// method's own declared return type against the interface's (confirmed by
	// `Bag::iter` below stating the concrete `Counter`, not `Iterator<int>`,
	// as its return type), so nothing about `iter()`'s return type actually
	// participates in resolving `T`.
	let src = r#"
		struct Counter(n: int, max: int)
		impl Iterator<int> for Counter {
			mut func next(): Option<int> = if (this.n > this.max) {
				None
			} else {
				let v = this.n
				this.n = this.n + 1
				Some(value = v)
			}
		}
		struct Bag(lo: int, hi: int)
		impl Iterable<int> for Bag {
			func iter(): Counter = Counter(n = this.lo, max = this.hi)
		}
		func sum_bag(): int = {
			let b = Bag(lo = 1, hi = 4)
			let mut total = 0
			for (x in b) {
				total = total + x
			}
			total
		}
	"#;
	// 1 + 2 + 3 + 4 = 10.
	assert_eq!(run(src, "sum_bag()"), "10");
}

#[test]
fn runs_a_for_loop_over_a_generic_iterable_bound() {
	let src = r#"
		struct Counter(n: int, max: int)
		impl Iterator<int> for Counter {
			mut func next(): Option<int> = if (this.n > this.max) { None } else {
				let value = this.n
				this.n = this.n + 1
				Some(value = value)
			}
		}
		struct Bag(max: int)
		impl Iterable<int> for Bag { func iter(): Counter = Counter(n = 1, max = this.max) }
		func sum<T: Iterable<T = int>>(items: T): int = {
			let mut total = 0
			for (item in items) { total = total + item }
			total
		}
		func demo(): int = sum(Bag(max = 4))
	"#;
	assert_eq!(run(src, "demo().v"), "10");
}

#[test]
fn runs_a_for_loop_over_a_spread_param_bound_to_a_list() {
	let src = r#"
		func total<Item>(...from: #[Item]): int = {
			let mut total = 0
			for (item in from) {
				total = total + 1
			}
			total
		}
		func demo(): int = total(...#[1, 2, 3, 4, 5])
	"#;
	assert_eq!(run(src, "demo().v"), "5");
}

// ── Slice 4I: `|>`, `in`/`!in`, `??` (Task 2) ────────────────────────────────

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
	// impl, only a plain method call.
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
		js.contains(".contains("),
		"expected a `.contains(` call in emitted JS:\n{js}"
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
	// user `Unwrap` overload compiles to a plain method call, never a native JS
	// `??` (which would test null/undefined — Nymph values are never
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
		js.contains(".unwrap("),
		"expected a `.unwrap(` call in emitted JS:\n{js}"
	);
	assert!(
		!js.contains("??"),
		"emitted JS must never contain a native `??` for a user Unwrap impl:\n{js}"
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
		js.matches("wrap(value) {").count() + js.matches("wrap (value) {").count(),
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
fn runs_impl_mut_method_mutates_a_this_field() {
	// FF2: the checker enforces nothing beyond an ordinary field assignment
	// for a `mut func` body, so lowering treats it as an ordinary instance
	// method — and emit now supports a `this.field = ..` assignment target
	// (previously an `unreachable!` reachable from a zero-diagnostic program).
	let src = r#"
		struct Counter(n: int) {
			mut func bump(): void = { this.n = this.n + 1 }
		}
		func run_bump(c: Counter): int = {
			c.bump()
			c.n
		}
	"#;
	assert_eq!(run(src, "run_bump(new Counter({ n: new NInt(5) }))"), "6");
}

#[test]
fn runs_field_slot_reassignment_gated_on_a_mut_receiver() {
	// Mutable types (MT1): `mut T` is compile-time-only — codegen is a near
	// no-op (JS objects are already mutable), so a program the checker lets
	// through because its receiver is `mut` must run under Node exactly like
	// the equivalent ungated field assignment always has.
	let src = r#"
		struct Counter(n: int)
		func bump(c: mut Counter): int = {
			c.n = c.n + 1
			c.n
		}
	"#;
	assert_eq!(run(src, "bump(new Counter({ n: new NInt(5) }))"), "6");
}

#[test]
fn runs_nested_mut_field_slot_reassignment_through_an_immutable_receiver() {
	// NN6, field-type-authority, pinned end to end: `inner`'s OWN declared
	// type (`mut Counter`) governs regardless of the outer `w`'s own
	// mutability — the checker lets `w.inner.n = ..` through even though `w`
	// itself is a plain, non-`mut` `Wrapper`, and (mut being compile-time-only)
	// the emitted JS mutates the shared `inner` object exactly as written.
	let src = r#"
		struct Counter(n: int)
		struct Wrapper(inner: mut Counter)
		func bump(w: Wrapper): int = {
			w.inner.n = w.inner.n + 1
			w.inner.n
		}
	"#;
	assert_eq!(
		run(
			src,
			"bump(new Wrapper({ inner: new Counter({ n: new NInt(5) }) }))"
		),
		"6"
	);
}

#[test]
fn runs_let_mut_reassignment_of_a_mut_typed_binding() {
	// `let mut` binds at `mut <ty(v)>` (NN4) — reassigning that binding, and
	// passing it on to a plain (non-`mut`) parameter (`mut T <: T`, one-way),
	// both run under Node exactly like the pre-mutable-types code they used to
	// be.
	let src = r#"
		func takes_int(x: int): int = x + 1
		func f(): int = {
			let mut n = 1
			n = 2
			takes_int(n)
		}
	"#;
	assert_eq!(run(src, "f()"), "3");
}

#[test]
fn runs_list_index_assignment() {
	// Confirmed defect (code review): `xs[i] = value` type-checks with zero
	// diagnostics (`infer_assign`'s field/index arm places no restriction on
	// an `IndexAccess` LHS) and used to panic in emit's `HirExpr::Assign`
	// match — an ICE (`unreachable!`) on valid input. A non-`Map` receiver's
	// index target now emits as a plain JS computed-member assignment.
	let src = r#"
		func set(xs: #[int], i: int, v: int): #[int] = {
			xs[i] = v
			xs
		}
	"#;
	assert_eq!(
		run(
			src,
			"JSON.stringify(nymphTestValue(set({ v: [{ v: 1 }, { v: 2 }, { v: 3 }] }, { v: 1 }, { v: 99 })))"
		),
		"[1,99,3]"
	);
}

#[test]
fn runs_map_index_assignment() {
	// Same defect, `Map` receiver: a JS `Map` has no assignment-expression
	// form for its entries (`m[k] = v` would silently set an own property on
	// the `Map` object itself, not mutate an entry), so this must lower to a
	// `.set(key, value)` call, not a computed-member `AssignmentTarget`.
	let src = r#"
		func set(m: #{int: int}, k: int, v: int): #{int: int} = {
			m[k] = v
			m
		}
	"#;
	assert_eq!(run(src, "set(new Map([[1, 10]]), 1, 99).get(1)"), "99");
}

// ── Slice 4K: `is`/`!is` desugar, `as` scalar/`Into` dispatch, end-to-end ──

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
fn runs_int_to_uint_cast_saturates_via_abs() {
	// Nymph defines its own semantics for `int as uint` rather than inheriting
	// JS/Rust edge behavior: no `Into` is declared anywhere for `as`, and JS
	// numbers can't express Rust's 2^64 wraparound, so a negative `int as uint`
	// takes `Math.abs` first — `int as uint` used to be a plain no-op (Slice 4K,
	// HH2); the abs-first rule makes it a real runtime operation now.
	let src = "func f(n: int): uint = n as uint";
	assert_eq!(run(src, "f(new NInt(5))"), "5");
	assert_eq!(run(src, "f(new NInt(-1))"), "1");
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
fn runs_float_to_uint_cast_takes_abs_then_truncates_toward_zero() {
	// `float as uint` takes `Math.abs` first, so a negative operand saturates to
	// its absolute value rather than staying negative.
	let src = "func f(x: float): uint = x as uint";
	assert_eq!(run(src, "f(new NFloat(3.7))"), "3");
	assert_eq!(run(src, "f(new NFloat(-3.7))"), "3");
}

#[test]
fn float_to_int_cast_saturates_nan_and_infinity() {
	// Nymph defines its own float→int semantics rather than inheriting JS's
	// (`Math.trunc` passes `NaN`/`±Infinity` straight through) or Rust's (`as`
	// saturates, but isn't reproducible on JS numbers as-is): `NaN` saturates to
	// `0`, and `±Infinity` saturate to `i64::MAX`/`i64::MIN` (JS stores the
	// former as `2^63`, the nearest `f64` to `2^63 - 1`).
	let src = "func f(x: float): int = x as int";
	assert_eq!(run(src, "f(new NFloat(NaN))"), "0");
	assert_eq!(run(src, "f(new NFloat(Infinity))"), "9223372036854776000");
	assert_eq!(run(src, "f(new NFloat(-Infinity))"), "-9223372036854776000");
}

#[test]
fn float_to_uint_cast_saturates_nan_and_both_infinities_to_the_same_max() {
	// `float as uint` takes `Math.abs` first, so `-Infinity` collapses onto the
	// same `Infinity -> i64::MAX` branch as `+Infinity` — there's no negative
	// saturation branch for an unsigned target.
	let src = "func f(x: float): int = x as int\nfunc g(x: float): uint = x as uint";
	assert_eq!(run(src, "g(new NFloat(NaN))"), "0");
	assert_eq!(run(src, "g(new NFloat(Infinity))"), "9223372036854776000");
	assert_eq!(run(src, "g(new NFloat(-Infinity))"), "9223372036854776000");
	// Sanity: the signed cast's `-Infinity` really is distinct from the unsigned
	// cast's (both derived from the same source function above).
	assert_eq!(run(src, "f(new NFloat(-Infinity))"), "-9223372036854776000");
}

#[test]
fn saturating_scalar_cast_evaluates_its_operand_exactly_once() {
	// The cast's arrow-IIFE must bind the operand to a gensym parameter and
	// reference *that*, never re-evaluate the source expression — otherwise a
	// side-effecting operand (a block that mutates `calls` every time it runs)
	// would run more than once per cast, once per branch that reads it.
	let src = "
		func f(): int = {
			let mut calls = 0
			let n = ({
				calls = calls + 1
				3.5
			}) as int
			calls
		}
		func g(): int = {
			let mut calls = 0
			let n = ({
				calls = calls + 1
				3.5
			}) as uint
			calls
		}
	";
	assert_eq!(run(src, "f()"), "1");
	assert_eq!(run(src, "g()"), "1");
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
fn numeric_to_char_evaluates_its_operand_exactly_once() {
	let src =
		"func f(): int = {\nlet mut calls = 0\nlet c = ({\ncalls = calls + 1\n65\n}) as char\ncalls\n}";
	assert_eq!(run(src, "f()"), "1");
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
	// Codegen invariant: a user `Into` dispatch must be a plain method call, never
	// anything referencing the built-in scalar-cast machinery.
	let src = r#"
		interface Into<Other> { func into(): Other }
		struct P(x: int)
		impl Into<string> for P { func into(): string = "p" }
		func f(p: P): string = p as string
	"#;
	let js = compile(src);
	assert!(
		js.contains(".into("),
		"expected a `.into(` call in emitted JS:\n{js}"
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
fn runs_a_closure_capturing_and_mutating_an_outer_mut_binding() {
	// JS arrows capture their enclosing scope by reference; the checker
	// permits assigning a captured outer `let mut` inside a closure body
	// (rejects the same assignment against a non-`mut` capture) — the
	// mutation must be visible to the enclosing function after the call.
	let src = "
		func f(): int = {
			let mut x = 1
			let bump = () -> { x = x + 1 }
			bump()
			x
		}
	";
	assert_eq!(run(src, "f()"), "2");
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
fn tuple_spread_evaluates_every_item_and_source_once_left_to_right() {
	let src = r#"
		struct Logger(count: int, trace: string) {
			mut func item(n: int): int = {
				this.count = this.count + 1
				this.trace = this.trace + "i"
				n
			}
			mut func pair(): #(boolean, string) = {
				this.count = this.count + 1
				this.trace = this.trace + "s"
				#(true, "x")
			}
		}
		func f(logger: mut Logger): #(int, boolean, string, int) =
			#(logger.item(1), ...logger.pair(), logger.item(2))
	"#;
	let js = r#"
		(() => {
			const logger = new Logger({ count: new NInt(0), trace: new NString("") });
			const value = f(logger);
			return JSON.stringify(nymphTestValue([value, logger.count, logger.trace]));
		})()
	"#;
	assert_eq!(run(src, js), r#"[[1,true,"x",2],3,"isi"]"#);
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
fn runs_a_list_spread_over_a_user_iterator_source() {
	// A non-array `Iterator<T>` source drains through Track A's own
	// `.next()`/`Option` protocol before splicing — no `Symbol.iterator`
	// involved.
	let src = r#"
		struct Counter(n: int, max: int)
		impl Iterator<int> for Counter {
			mut func next(): Option<int> = if (this.n > this.max) {
				None
			} else {
				let v = this.n
				this.n = this.n + 1
				Some(value = v)
			}
		}
		func f(): #[int] = {
			let mut c = Counter(n = 1, max = 3)
			#[...c, 99]
		}
	"#;
	assert_eq!(
		run(src, "JSON.stringify(nymphTestValue(f()))"),
		"[1,2,3,99]"
	);
}

#[test]
fn runs_a_list_spread_over_an_iterable_via_iter_source() {
	// The `Iterable<T>` (not `Iterator` itself) half of the protocol, via
	// `.iter()`, also drains correctly for a spread source.
	let src = r#"
		struct Counter(n: int, max: int)
		impl Iterator<int> for Counter {
			mut func next(): Option<int> = if (this.n > this.max) {
				None
			} else {
				let v = this.n
				this.n = this.n + 1
				Some(value = v)
			}
		}
		struct Bag(lo: int, hi: int)
		impl Iterable<int> for Bag {
			func iter(): Counter = Counter(n = this.lo, max = this.hi)
		}
		func f(): #[int] = {
			let b = Bag(lo = 1, hi = 3)
			#[0, ...b]
		}
	"#;
	assert_eq!(run(src, "JSON.stringify(nymphTestValue(f()))"), "[0,1,2,3]");
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
fn runs_a_map_spread_over_a_non_map_iterable_of_pairs() {
	// A non-map spread source may be a user `Iterator<#(K, V)>` of entry pairs,
	// which is drained and then merged.
	let src = r#"
		struct Pairs(n: int, max: int)
		impl Iterator<#(int, string)> for Pairs {
			mut func next(): Option<#(int, string)> = if (this.n > this.max) {
				None
			} else {
				let v = this.n
				this.n = this.n + 1
				Some(value = #(v, "x"))
			}
		}
		func f(): #{int: string} = {
			let mut p = Pairs(n = 1, max = 3)
			#{...p, 9: "z"}
		}
	"#;
	assert_eq!(
		run(
			src,
			"JSON.stringify(nymphTestValue(f()).sort(([a], [b]) => a - b))"
		),
		r#"[[1,"x"],[2,"x"],[3,"x"],[9,"z"]]"#
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
fn runs_a_map_spread_computed_key_eval_order() {
	// SS4: entries emit left-to-right in source order with no hoisting — a
	// side-effecting expression key evaluates exactly once, AFTER the spread
	// ahead of it, and a duplicate key it produces still wins over the
	// spread's own entry only because the literal comes LATER in the merged
	// entries array (`new Map` processes entries in order, later wins).
	let src = r#"
		struct Logger(count: int) {
			mut func record(n: int): int = {
				this.count = this.count + 1
				n
			}
		}
		func f(m: #{int: int}, logger: mut Logger): #{int: int} = #{...m, logger.record(1): 999}
	"#;
	let mutation_check = r#"
		const logger = new Logger({ count: new NInt(0) });
		const result = f(new Map([[new NInt(1), new NInt(10)], [new NInt(2), new NInt(20)]]), logger);
		return JSON.stringify(nymphTestValue([logger.count, result]));
	"#;
	assert_eq!(
		run(src, &format!("(() => {{ {mutation_check} }})()")),
		r#"[1,[[1,999],[2,20]]]"#
	);
}

// ── Collections materialization: extending `try_materialize_prelude_dispatch`
// to inherent `impl<T> #[T]`/`impl<K,V> #{K:V}` blocks (never scanned before
// this slice — it only ever handled `impl … for …`), so a pure-Nymph method
// resolved through a PRELUDE-ONLY inherent impl on `List`/`Map` materializes
// as a top-level `$std$$<tag>$<method>` function instead of panicking at
// `lower_hir.rs`'s "does not yet support dispatching a method call to a
// method resolved through a prelude-only impl" gate. ──────────────────────────

#[test]
fn runs_prelude_list_inherent_method_materializes_and_runs() {
	// The headline payoff: `second()` lives ONLY in a synthetic prelude
	// inherent impl on `#[T]` (never declared by the user module at all) —
	// before this slice, calling it panicked in lowering; now it materializes
	// to `$std$$list$second($self) => $self[1]` and actually runs under Node.
	let prelude = "impl<T> #[T] { func second(): T = this[1] }";
	let user = r#"
		func f(): int = {
			let xs = #[10, 20, 30]
			xs.second()
		}
	"#;
	assert_eq!(run_with_prelude(user, prelude, "f()"), "20");
}

#[test]
fn runs_prelude_mut_list_inherent_method_materializes_and_runs() {
	// Same mechanism, through an `impl<T> mut #[T]` block (the `mutable` flag
	// folds into the mangled tag as `mut_list`, distinct from the plain `list`
	// tag above, so the two never collide under the same mangled name). Reads
	// (not arithmetic) on the still-generic `T` — `a + b` on a bound generic
	// `T` is an unrelated, pre-existing GenericBound-dispatch limitation
	// (erased-generic `Plus` always compiles to `.plus(...)`, which a raw JS
	// number has no method for), nothing to do with this slice's gap.
	let prelude = "impl<T> mut #[T] { func first_elem(): T = this[0] }";
	let user = r#"
		func f(): int = {
			let mut xs = #[7, 1, 1]
			xs.first_elem()
		}
	"#;
	assert_eq!(run_with_prelude(user, prelude, "f()"), "7");
}

#[test]
fn runs_prelude_map_inherent_method_materializes_and_runs() {
	// Same mechanism, on `#{K: V}` — proves the `Map` arm of
	// `inherent_self_type_tag`, not just `List`.
	let prelude = "impl<K, V> #{K: V} { func answer(): int = 42 }";
	let user = r#"
		func f(): int = {
			let m = #{1: "a"}
			m.answer()
		}
	"#;
	assert_eq!(run_with_prelude(user, prelude, "f()"), "42");
}

/// Every real stdlib collection module, parsed once, as the FULL prelude
/// needed to typecheck `stdlib/src/collections/{list,map}.nym` in isolation
/// (their own `import`s name `Option`/`Plus`/`Into`/`Contains`/`Equals`, which
/// in turn need `Result`/`Default`/the rest of `ops` — imports are DROPPED by
/// `check_module_with_prelude`, not resolved, so every transitively-named
/// declaration must be supplied directly as a flattened prelude module,
/// mirroring `stdlib_check.rs`'s whole-stdlib acceptance test).
fn real_collections_prelude() -> &'static [nymph_ast::decl::Module] {
	static PRELUDE: OnceLock<Vec<nymph_ast::decl::Module>> = OnceLock::new();
	PRELUDE
		.get_or_init(load_real_collections_prelude)
		.as_slice()
}

fn load_real_collections_prelude() -> Vec<nymph_ast::decl::Module> {
	let stdlib_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
		.join("../../stdlib/src")
		.canonicalize()
		.unwrap();
	let mut files = Vec::new();
	fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
		for entry in std::fs::read_dir(dir).unwrap() {
			let path = entry.unwrap().path();
			if path.is_dir() {
				walk(&path, out);
			} else if path.extension().is_some_and(|e| e == "nym") {
				out.push(path);
			}
		}
	}
	walk(&stdlib_dir, &mut files);
	files.sort();
	files
		.iter()
		.map(|f| {
			let source = std::fs::read_to_string(f).unwrap();
			let parsed = parse_module(&source, f.to_str().unwrap());
			assert!(
				!parsed.diagnostics.iter().any(|d| d.is_error()),
				"{}: parse errors: {:?}",
				f.display(),
				parsed.diagnostics
			);
			parsed.tree
		})
		.collect()
}

/// Compile `user_src` against the FULL real stdlib as prelude ([`real_collections_prelude`]).
fn compile_against_real_stdlib(user_src: &str) -> String {
	let user = parse_module(user_src, "test");
	assert!(
		!user.diagnostics.iter().any(|d| d.is_error()),
		"parse errors in user source: {:?}",
		user.diagnostics
	);
	let prelude_modules = real_collections_prelude();
	let checked = check_module_with_prelude(&user.tree, prelude_modules);
	assert!(
		checked.diags.iter().all(|d| !d.is_error()),
		"check errors: {:?}",
		checked.diags
	);
	emit(&lower_hir_with_prelude(
		&user.tree,
		prelude_modules,
		&checked,
	))
}

#[test]
fn real_list_push_materializes_once_push_is_linked() {
	// `push` (`external(push)` in `list.nym`'s `impl<T> mut #[T]`) is now LINKED
	// for a mut-list receiver (`nymph_hir::linkage::REGISTRY`'s
	// `("push", Some("mut_list"))` row, L2), so `xs.push(1)` no longer
	// loud-defers: it lowers to `HirExpr::ExternCall` and emits a plain
	// `push($_this, 1)` call plus a deduped `import { push } from
	// "std/collections/list"`. Shape-only (the bare `emit` harness resolves no
	// imports); the bundle-path e2e in `nymph-compiler`'s `tests/std_linkage.rs`
	// proves it actually mutates + runs.
	let user = "func f(xs: mut #[int]): void = xs.push(1)";
	let js = compile_against_real_stdlib(user);
	assert!(
		js.contains("import { push as $nymph_external$call$"),
		"expected a linked-external import for `push`, got:\n{js}"
	);
	assert!(
		js.contains("push("),
		"expected `f`'s materialized body to call the linked `push`, got:\n{js}"
	);
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
	let js = compile_against_real_stdlib(src);
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
// be RUN directly — `compile_against_real_stdlib` uses the bare `emit`
// harness, which (per `run_node.rs`'s own module doc) never resolves imports,
// only string-appends a trailing `console.log`; running an unresolved
// `import` under plain `node` would throw `ERR_MODULE_NOT_FOUND`. So this
// asserts the emitted JS SHAPE instead of running it — the bundle-path e2e in
// `nymph-compiler`'s `tests/std_linkage.rs` proves the same mechanism actually
// RUNS, imports resolved and all.
#[test]
fn real_list_is_empty_materializes_once_length_is_linked() {
	let user = "func f(xs: #[int]): boolean = xs.is_empty()";
	let js = compile_against_real_stdlib(user);
	assert!(
		js.contains("import { length as $nymph_external$call$"),
		"expected a linked-external import for `length`, got:\n{js}"
	);
	assert!(
		js.matches("$nymph_external$call$").count() >= 2,
		"expected `is_empty`'s materialized body to call the linked `length`, got:\n{js}"
	);
	assert!(
		js.contains("is_empty"),
		"expected `is_empty` itself to materialize as a top-level function, got:\n{js}"
	);
}

// FLIP (Gap 3, L1): `list.nym`'s `get` is `external(get)` — it used to stay a
// loud defer for the identical reason `push` (above) still does: no JS
// binding anywhere for the marker. It is now LINKED for a `List` receiver
// (`nymph_hir::linkage::REGISTRY`'s `("get", Some("list"))`/`("get",
// Some("mut_list"))` rows, the Option ABI seam this slice wires), so it
// materializes: the call lowers to `HirExpr::ExternCall` carrying the
// ALREADY-resolved `(module, symbol)` pair (see `HirExpr::ExternCall`'s own
// doc comment for why — `get` is an AMBIGUOUS marker shared with `map.nym`'s
// own, different, `get`), which emits a plain `get($_this, i)` call plus a
// deduped `import { get } from "std/collections/list"`. Shape-only, same
// reasoning as `real_list_is_empty_materializes_once_length_is_linked` above
// — the bundle-path e2e in `nymph-compiler`'s `tests/std_linkage.rs` proves
// the mechanism actually RUNS (imports resolved, `Option` round-tripped
// through the user's own `match`).
#[test]
fn real_list_get_materializes_once_get_is_linked() {
	let user = "func f(xs: #[int]): Option<int> = xs.get(0)";
	let js = compile_against_real_stdlib(user);
	assert!(
		js.contains("import { get as $nymph_external$call$"),
		"expected a linked-external import for `get`, got:\n{js}"
	);
	assert!(
		js.matches("$nymph_external$call$").count() >= 2,
		"expected `f`'s materialized body to call the linked `get`, got:\n{js}"
	);
}

// FLIP (Gap 3, L3): `map.nym`'s `get` shares the SAME bare marker as
// `list.nym`'s (linked since L1, see
// `real_list_get_materializes_once_get_is_linked` above), but WAS a
// different, unlinked JS implementation — the registry's receiver-tag
// disambiguation (`Some("mut_map")`, since `map.nym` declares `get` inside
// its `impl<K,V> mut #{K:V}` block) is what previously kept a `Map`
// receiver's `get` a loud defer. L3 links it: it now materializes exactly
// like `list`'s `get`, into `HirExpr::ExternCall` emitting a plain
// `get($_this, key)` call plus a deduped `import { get } from
// "std/collections/map"`. Shape-only (same reasoning as the list flips
// above) — the bundle-path e2e in `nymph-compiler`'s `tests/std_linkage.rs`
// proves the mechanism actually RUNS.
#[test]
fn real_map_get_materializes_once_get_is_linked() {
	let user = "func f(m: mut #{int: int}): Option<int> = m.get(1)";
	let js = compile_against_real_stdlib(user);
	assert!(
		js.contains("import { get as $nymph_external$call$"),
		"expected a linked-external import for `get`, got:\n{js}"
	);
	assert!(
		js.matches("$nymph_external$call$").count() >= 2,
		"expected `f`'s materialized body to call the linked `get`, got:\n{js}"
	);
}

// FLIP (Gap 3, L3): `map.nym`'s `is_empty` (`this.size() == 0`) is
// transitively external through `size`, mirroring the list case above —
// `size` is now a LINKED (unambiguous, `receiver_tag: None`) external, so
// `body_calls_unlinked_external`'s registry subtraction no longer counts it
// as unlinked and `is_empty` materializes.
#[test]
fn real_map_is_empty_materializes_once_size_is_linked() {
	let user = "func f(m: #{int: int}): boolean = m.is_empty()";
	let js = compile_against_real_stdlib(user);
	assert!(
		js.contains("import { size as $nymph_external$call$"),
		"expected a linked-external import for `size`, got:\n{js}"
	);
	assert!(
		js.contains("is_empty"),
		"expected `is_empty` itself to materialize as a top-level function, got:\n{js}"
	);
}

// ── Named-type prelude method materialization: a prelude-only INSTANCE method
// on a NAMED enum receiver (`Option`/`Result`) now materializes ONTO that
// enum's own emitted class and RUNS, instead of panicking at the
// "prelude-only impl" wall above (that wall still stands for every OTHER
// unmaterializable shape — external/transitively-external collection
// intrinsics, a still-generic `GenericBound` receiver, and a genuinely
// unmaterializable body like `T.default()` through type erasure). See
// `compile_against_real_stdlib` for the full real-stdlib prelude, and
// `run_against_real_stdlib` below for the same shape driven under Node.

/// Same as [`compile_against_real_stdlib`], but the compiled JS runs under
/// Node (`run_js`) and the trimmed stdout is returned — the real-stdlib
/// counterpart of [`run_with_prelude`], for the named-type prelude method
/// materialization payoff.
fn run_against_real_stdlib(user_src: &str, call: &str) -> String {
	run_js(compile_against_real_stdlib(user_src), call)
}

#[test]
fn real_option_is_some_and_is_none_materialize_onto_the_option_class_and_run() {
	// `is_some`/`is_none` are `Option`'s own INLINE methods (`option.nym`);
	// `is_none = !this.is_some()` additionally exercises Sub-problem #1 (inner
	// dispatch): while `Option`'s class is being materialized, the `this.is_some()`
	// call inside `is_none`'s own body must resolve as a plain sibling method
	// call, not panic or re-route through the mangled-function path.
	let user = r#"
		func check(o: Option<int>): #(boolean, boolean) = #(o.is_some(), o.is_none())
	"#;
	assert_eq!(
		run_against_real_stdlib(
			user,
			"JSON.stringify(nymphTestValue(check(Option.Some({ value: new NInt(1) }))))"
		),
		"[true,false]"
	);
	assert_eq!(
		run_against_real_stdlib(user, "JSON.stringify(nymphTestValue(check(Option.None)))"),
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
		func unwrap_or(o: Option<int>): int = match (o) {
			Some(value) -> value,
			None -> 0,
		}
	"#;
	assert_eq!(
		run_against_real_stdlib(user, "unwrap_or(Option.Some({ value: 42 }))"),
		"42"
	);
	assert_eq!(run_against_real_stdlib(user, "unwrap_or(Option.None)"), "0");
}

#[test]
fn real_option_map_materializes_and_runs() {
	// `map` is another of `Option`'s own inline methods, this time taking a
	// closure argument (the sibling closure-lowering track's already-landed
	// machinery — untouched by this fix) and itself constructing a `Some` via
	// `VariantNew` inside the materialized body.
	let user = r#"
		func inc(o: Option<int>): Option<int> = o.map((x) -> x + 1)
	"#;
	assert_eq!(
		run_against_real_stdlib(user, "inc(Option.Some({ value: new NInt(1) })).value"),
		"2"
	);
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
		func get_or(o: Option<int>, default: int): int = o.unwrap(default)
	"#;
	assert_eq!(
		run_against_real_stdlib(user, "get_or(Option.Some({ value: 7 }), 0)"),
		"7"
	);
	assert_eq!(run_against_real_stdlib(user, "get_or(Option.None, 9)"), "9");
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
		func ok_is_some(r: Result<int, string>): boolean = r.ok().is_some()
		func err_value(r: Result<int, string>): string = match (r.err()) {
			Some(value) -> value,
			None -> "no error",
		}
	"#;
	assert_eq!(
		run_against_real_stdlib(user, "ok_is_some(Result.Ok({ value: 5 }))"),
		"true"
	);
	assert_eq!(
		run_against_real_stdlib(user, "err_value(Result.Error({ error: 'boom' }))"),
		"boom"
	);
}

#[test]
#[should_panic(
	expected = "does not yet support a namespaced call through a generic type parameter"
)]
fn real_option_map_or_default_stays_a_loud_defer_even_on_demand() {
	// The honest floor this slice's demand-only approach exists to preserve:
	// `Option`'s own `map_or_default` (`option.nym`) calls `R.default()`
	// through a still-generic type parameter, which has no compilable JS form
	// under type erasure — demand-only lowering means this is NEVER reached
	// merely because `Option` is referenced (the tests above never hit it),
	// but a program that actually CALLS `map_or_default` still demands it,
	// and still hits this same pre-existing generic-namespaced-call panic
	// once it's lowered.
	//
	// `int` already implements the real stdlib's own `Default` (`default.nym`,
	// part of the same prelude walk), so no extra declaration is needed here.
	let user = r#"
		func get(o: Option<int>): int = o.map_or_default((x) -> x)
	"#;
	let _ = compile_against_real_stdlib(user);
}

#[test]
fn real_range_contains_emits_generic_comparison_dispatch() {
	let user = r#"
		func in_range(x: int): boolean = {
			let r = Range(start = 0, end = 5)
			r.contains(x)
		}
	"#;
	let js = compile_against_real_stdlib(user);
	assert!(js.contains("Symbol.for(\"nymph.int\")"), "{js}");
	assert!(js.contains("$std$Comparable$int$less_than_eq"), "{js}");
	assert!(js.contains("$std$Comparable$int$greater_than"), "{js}");
}

// ── Structural-collection interface-impl materialization (`ImplFor` targeting
// `#[T]`/`#{K:V}`): extending `try_materialize_prelude_dispatch`'s `ImplFor`
// branch to tag a structural receiver the same way the inherent `Impl` branch
// already does (Gap 1), and giving an interface default body materialized as
// a top-level mangled function a way to resolve an INNER sibling-interface-
// method call against the SAME concrete impl the outer call already found
// (Gap 2) — previously both panicked at the "prelude-only impl" wall, even
// though the bodies involved are pure Nymph with no `external` anywhere
// (Gap 3, the stdlib linkage wall, is deliberately untouched — see the
// `real_*_stays_a_loud_*_defer` tests above, which must keep panicking). ──

#[test]
fn runs_prelude_interface_own_method_on_list_materializes_and_runs() {
	// Gap 1: `own_pure` lives only in an `impl<T> SomeIface for #[T]` block
	// (an interface impl targeting a STRUCTURAL list type, never scanned by
	// the `ImplFor` branch's old `primitive_type_tag` call, which returns
	// `None` for `Type::List`). Before the fix this panicked in lowering;
	// now it tags as `list` via `inherent_self_type_tag` (the same tag the
	// INHERENT `impl<T> #[T]` branch already used) and materializes to
	// `$std$SomeIface$list$own_pure($self) => 42`.
	let prelude = r#"
		interface SomeIface {
			func own_pure(): int
		}
		impl<T> SomeIface for #[T] {
			func own_pure(): int = 42
		}
	"#;
	let user = r#"
		func f(): int = {
			let xs = #[10, 20, 30]
			xs.own_pure()
		}
	"#;
	assert_eq!(run_with_prelude(user, prelude, "f()"), "42");
}

#[test]
fn runs_prelude_interface_own_method_on_map_materializes_and_runs() {
	// Same mechanism as above, on `#{K: V}` — proves the `Map` arm of
	// `inherent_self_type_tag` through the `ImplFor` branch too, not just
	// `List` or the pre-existing inherent-`Impl` branch.
	let prelude = r#"
		interface SomeIface {
			func own_pure(): int
		}
		impl<K, V> SomeIface for #{K: V} {
			func own_pure(): int = 42
		}
	"#;
	let user = r#"
		func f(): int = {
			let m = #{1: "a"}
			m.own_pure()
		}
	"#;
	assert_eq!(run_with_prelude(user, prelude, "f()"), "42");
}

#[test]
fn runs_prelude_interface_default_calling_sibling_method_on_list_materializes_and_runs() {
	// Gap 2: `doubled` is `SomeIface`'s own DEFAULT body, calling the
	// interface's own required `base()` through `this` — required, so this
	// impl block must (and does) provide its own `base`, not fall back to
	// another default. The checker types `doubled`'s `this.base()` call
	// generically against `SomeIface`'s own synthetic `this`, so that inner
	// call's `impl_span` names the INTERFACE declaration, never this
	// concrete `impl<T> SomeIface for #[T]` block — the ordinary span-scan
	// in `try_materialize_prelude_dispatch` can never match it. Before the
	// fix this panicked mid-materialization (`base` "not yet supported");
	// now `lower_runtime_func` pushes a sibling-dispatch frame while
	// lowering `doubled`'s own body, so the inner call resolves directly to
	// `$std$SomeIface$list$base` — the SAME mangled name a direct outer call
	// to `.base()` would produce — and `.doubled()` runs to `21 + 21 = 42`.
	let prelude = r#"
		interface SomeIface {
			func base(): int
			func doubled(): int = this.base() + this.base()
		}
		impl<T> SomeIface for #[T] {
			func base(): int = 21
		}
	"#;
	let user = r#"
		func f(): int = {
			let xs = #[10, 20, 30]
			xs.doubled()
		}
	"#;
	assert_eq!(run_with_prelude(user, prelude, "f()"), "42");
}

#[test]
fn runs_prelude_interface_default_calling_another_default_on_list_materializes_and_runs() {
	// Gap 2 variant: `doubled`'s sibling `base` is ALSO a default (not
	// overridden by this impl block at all — the `impl<T> SomeIface for
	// #[T] { }` body is empty), exercising the sibling-frame fallback's own
	// interface-default lookup (`resolve_impl_for_source`'s `own_member.or_else`
	// branch), not just an impl-provided override.
	let prelude = r#"
		interface SomeIface {
			func base(): int = 21
			func doubled(): int = this.base() + this.base()
		}
		impl<T> SomeIface for #[T] {
		}
	"#;
	let user = r#"
		func f(): int = {
			let xs = #[10, 20, 30]
			xs.doubled()
		}
	"#;
	assert_eq!(run_with_prelude(user, prelude, "f()"), "42");
}

#[test]
fn runs_prelude_interface_default_calling_sibling_method_on_map_materializes_and_runs() {
	// Gap 2, `Map` receiver variant — same mechanism as the list case, on
	// the other structural collection type.
	let prelude = r#"
		interface SomeIface {
			func base(): int
			func doubled(): int = this.base() + this.base()
		}
		impl<K, V> SomeIface for #{K: V} {
			func base(): int = 21
		}
	"#;
	let user = r#"
		func f(): int = {
			let m = #{1: "a"}
			m.doubled()
		}
	"#;
	assert_eq!(run_with_prelude(user, prelude, "f()"), "42");
}

#[test]
#[should_panic(
	expected = "does not yet support dispatching a method call to a method resolved through a prelude-only impl"
)]
fn real_set_insert_stays_a_loud_transitively_external_defer() {
	// `Set.insert` (`this.inner.insert(item, #())`, set.nym) calls the real Map's
	// own `insert`, which is `external` (map.nym) — the same pre-existing
	// prelude-method-materialization gap `real_map_get_stays_a_loud_external_defer`
	// pins for Map directly, confirmed here to reach transitively through Set too.
	// Out of THIS slice's scope: closing it requires touching `lower_hir.rs`,
	// owned by another in-flight slice (see that file's task-scope note). A
	// genuine `Set` insert/remove/contains round-trip against the REAL stdlib
	// cannot run under Node until that separate gap closes — confirmed by probe,
	// not fixed here.
	let user = "func f(): boolean = {\n\tlet s = Set(inner = #{})\n\ts.insert(1)\n}";
	let _ = compile_against_real_stdlib(user);
}

// ── Owned collection literal → `mut` coercion (Bug 2), driven under Node ────
//
// `real_set_insert_stays_a_loud_transitively_external_defer` above shows the
// REAL stdlib's `Set`/`Map` mutating methods can't run yet (an unrelated,
// pre-existing gap). These use a self-contained synthetic setup — native `[]`
// index read/assign on `#{…}`/`#[…]`, which lowers to a plain JS `Map`/`Array`
// with no `external` linkage involved (`emit.rs`'s `HirExpr::Assign` arm) — to
// prove the FIX itself: a fresh collection literal accepted at a `mut`
// parameter/ctor field type-checks AND the emitted JS actually mutates the
// SAME literal the caller passed in.

#[test]
fn a_fresh_map_literal_at_a_mut_parameter_is_mutated_and_read_back() {
	let user =
		"func take(m: mut #{int: int}): int = {\n\tm[1] = 99\n\tm[1]\n}\nfunc t(): int = take(#{1: 2})";
	assert_eq!(run(user, "t()"), "99");
}

#[test]
fn a_fresh_list_literal_at_a_mut_parameter_is_mutated_and_read_back() {
	let user = "func take(xs: mut #[int]): int = {\n\txs[0] = 99\n\txs[0]\n}\nfunc t(): int = take(#[1, 2, 3])";
	assert_eq!(run(user, "t()"), "99");
}

#[test]
fn a_fresh_map_literal_at_a_mut_struct_ctor_field_is_mutated_and_read_back() {
	let user = "struct Box(m: mut #{int: int}) {}\nfunc t(): int = {\n\tlet b = Box(m = #{1: 2})\n\tb.m[1] = 99\n\tb.m[1]\n}";
	assert_eq!(run(user, "t()"), "99");
}

// ── Unannotated if/block-bodied inherent method return type (Bug 1 regression
// guard), driven under Node ─────────────────────────────────────────────────

#[test]
fn an_unannotated_inherent_method_with_an_if_block_body_runs_and_returns_the_branches_common_type()
{
	let user = "struct Wrapper(flag: boolean) {}\nimpl Wrapper {\n\tmut func toggle(cond: boolean) = if (cond) {\n\t\tthis.flag = true\n\t\ttrue\n\t} else false\n}\nfunc t(cond: boolean): boolean = {\n\tlet mut w = Wrapper(flag = false)\n\tw.toggle(cond)\n}";
	assert_eq!(run(user, "t(new NBool(true))"), "true");
	assert_eq!(run(user, "t(new NBool(false))"), "false");
}

// ── Mutable-field projection + still-generic bound dispatch (iterator-adapter
// groundwork), driven under Node ─────────────────────────────────────────────

#[test]
fn a_mut_func_can_call_a_mut_method_on_a_concrete_field_and_it_runs() {
	// Projecting a field out of a `mut` receiver yields a mutable place, so `step`
	// (a `mut func`) may call `bump` (also `mut func`) on `this.inner`. Before the
	// mutable-field-projection fix this failed to type-check ("bump requires a mut
	// receiver"); now it runs, and the two `bump`s mutate shared state (0 → 2).
	let user = "struct Inner(n: int) {\n\tmut func bump(): int = {\n\t\tthis.n = this.n + 1\n\t\tthis.n\n\t}\n}\nstruct Outer(inner: Inner) {\n\tmut func step(): int = this.inner.bump()\n}\nfunc t(): int = {\n\tlet mut o = Outer(inner = Inner(n = 0))\n\tlet a = o.step()\n\tlet b = o.step()\n\ta + b\n}";
	assert_eq!(run(user, "t()"), "3");
}

#[test]
fn a_lazy_map_adapter_over_a_generic_iterator_source_runs() {
	// The full lazy-adapter shape: `Map` is generic over its source iterator
	// (`S: Iterator<Item>`) and, inside its own `next`, calls `this.source.next()`
	// — a `mut` method on a generic FIELD, dispatched through the bound. That relies
	// on BOTH fixes: mutable-field projection (so `this.source` is a mutable place)
	// and still-generic bound lowering (so `next` emits a plain dynamic call rather
	// than panicking as an unmaterializable prelude impl). Consuming `doubled` in a
	// for-loop yields 0,2,4,6 → sum 12.
	let prelude = "enum Option<T> {\n\tSome(value: T),\n\tNone\n\n\tfunc map<R>(f: (T) -> R): Option<R> = match (this) {\n\t\tSome(value) -> Option.Some(value = f(value)),\n\t\tNone -> Option.None\n\t}\n}\ninterface Iterator<Item> {\n\tmut func next(): Option<Item>\n}";
	let user = "struct Map<Item, R, S: Iterator<Item>>(source: S, f: (Item) -> R) {\n\timpl Iterator<R> {\n\t\tmut func next(): Option<R> = this.source.next().map(this.f)\n\t}\n}\nstruct Counter(current: uint, limit: uint) {\n\timpl Iterator<uint> {\n\t\tmut func next(): Option<uint> = if (this.current < this.limit) {\n\t\t\tlet value = this.current\n\t\t\tthis.current = this.current + 1u\n\t\t\tOption.Some(value = value)\n\t\t} else {\n\t\t\tOption.None\n\t\t}\n\t}\n}\nfunc f(): uint = {\n\tlet c = Counter(current = 0u, limit = 4u)\n\tlet mut doubled = Map(source = c, f = (n) -> n * 2u)\n\tlet mut total = 0u\n\tfor (x in doubled) {\n\t\ttotal = total + x\n\t}\n\ttotal\n}";
	assert_eq!(run_with_prelude(user, prelude, "f()"), "12");
}

#[test]
fn a_default_map_method_on_the_ambient_iterator_interface_materializes_its_adapter_and_runs() {
	// The user-facing directive shape: `map` is a DEFAULT method on the AMBIENT
	// `Iterator` interface, returning a lazy `MapAdapter<Item, R, self>` that also lives
	// in the prelude. Calling `c.map(f)` on a concrete `Counter` materializes the default
	// body onto the emitted class — and the referenced `MapAdapter` struct, emitted
	// nowhere in the user module, is demand-materialized from the prelude (its own `next`
	// then chains through the generic source). 0,2,4,6 → sum 12.
	let prelude = "enum Option<T> {\n\tSome(value: T),\n\tNone\n\n\tfunc map<R>(f: (T) -> R): Option<R> = match (this) {\n\t\tSome(value) -> Option.Some(value = f(value)),\n\t\tNone -> Option.None\n\t}\n}\nstruct MapAdapter<Item, R, S: Iterator<Item>>(source: S, f: (Item) -> R) {\n\timpl Iterator<R> {\n\t\tmut func next(): Option<R> = this.source.next().map(this.f)\n\t}\n}\ninterface Iterator<Item> {\n\tmut func next(): Option<Item>\n\n\tfunc map<R>(f: (Item) -> R): MapAdapter<Item, R, self> = MapAdapter(source = this, f = f)\n}";
	let user = "struct Counter(current: uint, limit: uint) {\n\timpl Iterator<uint> {\n\t\tmut func next(): Option<uint> = if (this.current < this.limit) {\n\t\t\tlet value = this.current\n\t\t\tthis.current = this.current + 1u\n\t\t\tOption.Some(value = value)\n\t\t} else {\n\t\t\tOption.None\n\t\t}\n\t}\n}\nfunc f(): uint = {\n\tlet c = Counter(current = 0u, limit = 4u)\n\tlet mut doubled = c.map((n) -> n * 2u)\n\tlet mut total = 0u\n\tfor (x in doubled) {\n\t\ttotal = total + x\n\t}\n\ttotal\n}";
	assert_eq!(run_with_prelude(user, prelude, "f()"), "12");
}

#[test]
fn an_ambient_iterator_draining_terminal_default_iterates_this_and_runs() {
	// A draining terminal (`count`) is a `mut func` DEFAULT on the ambient `Iterator`
	// whose body does `for (item in this)`. That exercises: `this` bound as `mut Self`
	// in the default body; the for-loop over a `Param` bound by `Iterator` resolving to
	// the `.next()` protocol (not the native-list index walk); and the materialized
	// default's own operator (`n + 1u`). `count()` over 0..6 → 6.
	let prelude = r#"enum Option<T> {
	Some(value: T),
	None
}
interface Iterator<Item> {
	mut func next(): Option<Item>

	mut func count(): uint = {
		let mut n = 0u
		for (item in this) {
			n = n + 1u
		}
		n
	}

	mut func to_list(): #[Item] = {
		let mut out: #[Item] = #[]
		for (item in this) {
			out.push(item)
		}
		out
	}
}"#;
	let user = r#"struct Counter(current: uint, limit: uint) {
	impl Iterator<uint> {
		mut func next(): Option<uint> = if (this.current < this.limit) {
			let value = this.current
			this.current = this.current + 1u
			Option.Some(value = value)
		} else {
			Option.None
		}
	}
}
func f(): uint = {
	let mut c = Counter(current = 0u, limit = 6u)
	c.count()
}
func g(): #[uint] = {
	let mut c = Counter(current = 0u, limit = 4u)
	c.to_list()
}"#;
	assert_eq!(run_with_prelude(user, prelude, "f()"), "6");
	assert_eq!(run_with_prelude(user, prelude, "g()"), "[ 0, 1, 2, 3 ]");
}

#[test]
fn a_terminal_chained_onto_a_lazy_map_adapter_runs() {
	// `c.map(f).to_list()` / `.count()` — a draining terminal called on a `Mapped`
	// adapter TEMPORARY (a demand-materialized core prelude struct). The terminal is an
	// inherited `Iterator` default on `Mapped`; dispatching it as a plain call on the
	// materialized adapter class (rather than a loud "prelude-only impl" defer) is the
	// chaining payoff. `map(n -> n*n)` over 0..4 → to_list [0,1,4,9]; `.count()` → 4.
	let prelude = r#"enum Option<T> {
	Some(value: T),
	None

	func map<R>(f: (T) -> R): Option<R> = match (this) {
		Some(value) -> Option.Some(value = f(value)),
		None -> Option.None
	}
}
struct Mapped<Item, R, S: Iterator<Item>>(source: S, f: (Item) -> R) {
	impl Iterator<R> {
		mut func next(): Option<R> = this.source.next().map(this.f)
	}
}
interface Iterator<Item> {
	mut func next(): Option<Item>

	func map<R>(f: (Item) -> R): Mapped<Item, R, self> = Mapped(source = this, f = f)

	mut func to_list(): #[Item] = {
		let mut out: #[Item] = #[]
		for (item in this) {
			out.push(item)
		}
		out
	}

	mut func count(): uint = {
		let mut n = 0u
		for (item in this) {
			n = n + 1u
		}
		n
	}
}"#;
	let user = r#"struct Counter(current: uint, limit: uint) {
	impl Iterator<uint> {
		mut func next(): Option<uint> = if (this.current < this.limit) {
			let value = this.current
			this.current = this.current + 1u
			Option.Some(value = value)
		} else {
			Option.None
		}
	}
}
func squares(): #[uint] = {
	let mut c = Counter(current = 0u, limit = 4u)
	c.map((n) -> n * n).to_list()
}
func how_many(): uint = {
	let mut c = Counter(current = 0u, limit = 4u)
	c.map((n) -> n * n).count()
}"#;
	assert_eq!(
		run_with_prelude(user, prelude, "squares()"),
		"[ 0, 1, 4, 9 ]"
	);
	assert_eq!(run_with_prelude(user, prelude, "how_many()"), "4");
}

#[test]
fn filter_take_drop_adapters_chain_and_run() {
	// `filter` (a `while`-looping adapter calling a closure FIELD), `take`, and `drop`
	// as ambient `Iterator` defaults, chained with `map`: 0..20 → evens → ×10 → first 4.
	let prelude = r#"enum Option<T> {
	Some(value: T),
	None

	func map<R>(f: (T) -> R): Option<R> = match (this) {
		Some(value) -> Option.Some(value = f(value)),
		None -> Option.None
	}
}
struct Mapped<Item, R, S: Iterator<Item>>(source: S, f: (Item) -> R) {
	impl Iterator<R> {
		mut func next(): Option<R> = this.source.next().map(this.f)
	}
}
struct Filtered<Item, S: Iterator<Item>>(source: S, predicate: (Item) -> boolean) {
	impl Iterator<Item> {
		mut func next(): Option<Item> = {
			let keep = this.predicate
			let mut found: Option<Item> = Option.None
			let mut searching = true
			while (searching) {
				match (this.source.next()) {
					Some(value) -> if (keep(value)) {
						found = Option.Some(value = value)
						searching = false
					} else {},
					None -> { searching = false },
				}
			}
			found
		}
	}
}
struct Take<Item, S: Iterator<Item>>(source: S, remaining: uint) {
	impl Iterator<Item> {
		mut func next(): Option<Item> = if (this.remaining > 0u) {
			this.remaining = this.remaining - 1u
			this.source.next()
		} else {
			Option.None
		}
	}
}
struct Drop<Item, S: Iterator<Item>>(source: S, remaining: uint) {
	impl Iterator<Item> {
		mut func next(): Option<Item> = {
			while (this.remaining > 0u) {
				this.remaining = this.remaining - 1u
				this.source.next()
			}
			this.source.next()
		}
	}
}
interface Iterator<Item> {
	mut func next(): Option<Item>

	func map<R>(f: (Item) -> R): Mapped<Item, R, self> = Mapped(source = this, f = f)
	func filter(predicate: (Item) -> boolean): Filtered<Item, self> = Filtered(source = this, predicate = predicate)
	func take(n: uint): Take<Item, self> = Take(source = this, remaining = n)
	func drop(n: uint): Drop<Item, self> = Drop(source = this, remaining = n)

	mut func to_list(): #[Item] = {
		let mut out: #[Item] = #[]
		for (item in this) {
			out.push(item)
		}
		out
	}
}"#;
	let user = r#"struct Counter(current: uint, limit: uint) {
	impl Iterator<uint> {
		mut func next(): Option<uint> = if (this.current < this.limit) {
			let value = this.current
			this.current = this.current + 1u
			Option.Some(value = value)
		} else {
			Option.None
		}
	}
}
func evens(): #[uint] = {
	let mut c = Counter(current = 0u, limit = 10u)
	c.filter((n) -> n % 2u == 0u).to_list()
}
func dropped(): #[uint] = {
	let mut c = Counter(current = 0u, limit = 10u)
	c.drop(7u).to_list()
}
func chained(): #[uint] = {
	let mut c = Counter(current = 0u, limit = 20u)
	c.filter((n) -> n % 2u == 0u).map((n) -> n * 10u).take(4u).to_list()
}"#;
	assert_eq!(
		run_with_prelude(user, prelude, "evens()"),
		"[ 0, 2, 4, 6, 8 ]"
	);
	assert_eq!(run_with_prelude(user, prelude, "dropped()"), "[ 7, 8, 9 ]");
	assert_eq!(
		run_with_prelude(user, prelude, "chained()"),
		"[ 0, 20, 40, 60 ]"
	);
}

// NOTE: `#[T]` list iteration (`xs.iter().map(..).to_list()` via the `Iterable` impl on
// `#[T]` + `ListIter`) can't be exercised here — `ListIter::next` calls the LINKED
// `list.get` external, which the `run_with_prelude` harness (a hand-written prelude, no
// stdlib linkage) doesn't provide. It is covered by `stdlib_check` (type-checks) and end
// to end by the CLI against the real stdlib.

// ── Positional sub-patterns on single-field constructors ─────────────────────

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
fn pattern_binding_evaluates_the_scrutinee_once() {
	let src = "struct Counter(n: int) {}\nimpl Counter {\n\tmut func next(): #(int, int) = {\n\t\tthis.n = this.n + 1\n\t\t#(1, 7)\n\t}\n}\nfunc capture(): #(int, int, int) = {\n\tlet mut counter = Counter(n = 0)\n\tmatch (counter.next()) {\n\t\twhole = #(left, _) -> #(counter.n as int, whole[0], left),\n\t}\n}";
	assert_eq!(run(src, "capture()"), "[ 1, 1, 1 ]");
}

#[test]
fn break_value_and_continue_cross_expression_iifes() {
	let src = r#"func branch(): int = match (while (true) { if (true) { break 7 } else { continue } }) {
		Some(value) -> value,
		None -> 0,
	}"#;
	assert_eq!(run(src, "branch()"), "7");
}

#[test]
fn loop_natural_exhaustion_and_bare_break_use_option_abi() {
	let src = r#"func exhausted(): int = match (while (false) { break 1 }) {
		Some(value) -> value,
		None -> 4,
	}
func bare(): int = match (while (true) { break }) {
		Some(#()) -> 5,
		None -> 0,
	}"#;
	assert_eq!(run(src, "exhausted()"), "4");
	assert_eq!(run(src, "bare()"), "5");
}

#[test]
fn loop_control_targets_for_and_nested_loops_and_evaluates_positions_once() {
	let src = r#"
func keep(value: int): int = value

func positioned(): int = {
  let mut hits = 0
  let result = while (true) {
    hits = hits + 1
    keep(10 + if (hits == 1) { continue } else { break hits })
  }
  match (result) { Some(value) -> hits * 10 + value, None -> 0 }
}

func for_results(): int = {
  let early = for (value in 1..4) { if (value == 2) { break value } }
  let natural = for (value in 1..3) { if (value == 9) { break value } }
  match (early) {
    Some(value) -> value * 10 + match (natural) { Some(_) -> 1, None -> 0 },
    None -> 0,
  }
}

func range_continue(): int = {
  let mut total = 0
  for (value in 1..=5) {
    if (value == 3) { continue }
    total = total + value
  }
  total
}

func nested(): int = match (while (true) {
  let inner = while (true) { break 4 }
  break match (inner) { Some(value) -> value + 1, None -> 0 }
}) { Some(value) -> value, None -> 0 }

func match_arm(): int = match (while (true) {
  match (1) { 1 -> break 8, _ -> continue }
}) { Some(value) -> value, None -> 0 }

func short_circuit(): int = {
  let mut hits = 0
  let result = while (true) {
    hits = hits + 1
    false && continue
    true || continue
    if (hits == 1) { true && continue }
    false || break hits
  }
  match (result) { Some(value) -> hits * 10 + value, None -> 0 }
}

func eager_positions(): int = {
  let prefix = while (true) { -(break 1) }
  let callee = while (true) { (break 2)() }
  let member = while (true) { (break 3).field }
  let index = while (true) { #[0][break 4] }
  let cast = while (true) { (break 5) as int }
  match (prefix) { Some(a) -> match (callee) { Some(b) -> match (member) {
    Some(c) -> match (index) { Some(d) -> match (cast) { Some(e) -> a + b + c + d + e, None -> 0 }, None -> 0 },
    None -> 0,
  }, None -> 0 }, None -> 0 }
}
"#;
	assert_eq!(run(src, "positioned()"), "22");
	assert_eq!(run(src, "for_results()"), "20");
	assert_eq!(run(src, "range_continue()"), "12");
	assert_eq!(run(src, "nested()"), "5");
	assert_eq!(run(src, "match_arm()"), "8");
	assert_eq!(run(src, "short_circuit()"), "22");
	assert_eq!(run(src, "eager_positions()"), "15");
}

#[test]
fn return_crosses_option_loop_expression_iife_in_both_lowering_paths() {
	let src = r#"
func choose(flag: boolean): int = {
	let result = while (true) {
		if (flag) { return 9 }
		break 4
	}
	match (result) { Some(value) -> value, None -> 0 }
}

func nested(): int = {
	let choose = (flag: boolean) -> {
		let result = while (true) {
			if (flag) { return 8 }
			break 3
		}
		match (result) { Some(value) -> value, None -> 0 }
	}
	choose(true) * 10 + choose(false)
}
"#;
	assert_eq!(run(src, "choose(new NBool(true))"), "9");
	assert_eq!(run(src, "choose(new NBool(false))"), "4");
	assert_eq!(run(src, "nested()"), "83");

	let prelude = "enum Option<T> { Some(value: T), None }";
	assert_eq!(
		run_with_prelude(src, prelude, "choose(new NBool(true))"),
		"9"
	);
	assert_eq!(
		run_with_prelude(src, prelude, "choose(new NBool(false))"),
		"4"
	);
	assert_eq!(run_with_prelude(src, prelude, "nested()"), "83");
}

#[test]
fn loop_completion_does_not_swallow_user_exceptions() {
	let src = r#"
func invoke(callback: () -> int): Option<int> = while (true) {
	callback()
	break 1
}
"#;
	let stderr = run_failure(src, "invoke(() => { throw new Error('user boom') })");
	assert!(stderr.contains("user boom"), "{stderr}");
}
