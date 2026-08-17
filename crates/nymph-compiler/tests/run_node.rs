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

#[test]
fn runs_arithmetic() {
	// Assert that pure scalar arithmetic runs, in addition to emission/lowering coverage.
	let out = run(
		"func add(a: int, b: int): int = a + b * 2",
		"add(new NInt(3), new NInt(4))",
	);
	assert_eq!(out, "11");
}

#[test]
fn runs_an_operator_inside_a_string_interpolation() {
	// Interpolated expressions must share the surrounding parser's node-id
	// sequence. A fresh sub-parser creates collisions that clobber recorded
	// operator dispatches, surfacing as "no operator resolution recorded". The
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
	let src = r#"
		func at(i: int): int = #[10, 20, 30][i]
		func at_unsigned(i: uint): int = #[10, 20, 30][i]
	"#;
	assert_eq!(run(src, "at(new NInt(-1))"), "30");
	assert_eq!(run(src, "at_unsigned(new NUint(1))"), "20");
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
	// Int keys isolate map indexing from string-literal lowering.
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
	// limitation that lowering supports only identifier parameters — unrelated
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
	// body) dispatches to `.plus(...)` rather than a native JS `+`.
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
	// (which would silently string-coerce two class instances).
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
	// `.negate()` at runtime, componentwise negating the vector.
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
	// `.bit_not()` at runtime, componentwise bit-negating the mask.
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

// ── Interface default method materialization ───────────────────────────────

#[test]
fn runs_interface_default_dispatches_via_operator() {
	// `v1 < v2` desugars to `Comparable::less_than`, which `Vec2` never defines
	// directly — only `compare_to`. Lowering materializes `less_than`'s
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
	// (`v.less_than(w)`) rather than through the `<` operator. The default must
	// materialize as a real class method so lowered JS cannot call a missing method.
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
	// constant `false`), not the materialized default; overrides always win.
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

// ── Comparison/equality generics end-to-end ────────────────────────────────

#[test]
fn runs_late_pinned_adt_comparison_dispatches_at_runtime() {
	// The headline silent-miscompile probe, run for real under Node: `xs[0] <
	// xs[0]` is recorded against a still-unbound inference variable, later
	// pinned to `Vec2` by the `#[Vec2]` annotation. It must not compile to a
	// native JS `<` between two class instances (`NaN`-ish nonsense);
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
	// The concrete-primitive fast path remains untouched: `int`/`float`
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
	// checker rejects direct `this.field` access, so matching is the supported
	// way to inspect `this` inside an enum method).
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
	// exactly like the struct case because enums can carry methods.
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
	// The tag-identity value ABI must not change for a methodful enum:
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

// ── `return`, let-shadowing, module lets ───────────────────────────────────

#[test]
fn runs_early_return_with_value_inside_a_statement_position_if() {
	// The corpus `abs` shape: an early `return n` inside a statement-position
	// `if`, falling through to the trailing expression otherwise.
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
	// an unnecessary IIFE — the `while` body remains in statement position.
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
	// and its RHS reads the prior `x`.
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
	// Both bindings must not emit as the identical JS
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
fn mutable_callable_initializer_calls_remain_compile_errors() {
	let src = "let mut callback = () -> 5\nlet result = callback()";
	let result = nymph_compiler::compile(src, "test");
	let diagnostics = result.expect_err("mutable callable initializer must not produce JavaScript");
	assert!(
		diagnostics
			.iter()
			.any(|diagnostic| diagnostic.message.contains("UnresolvedInitializerCall")),
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
	assert_eq!(js.matches("const result = callback();").count(), 1, "{js}");
	assert_eq!(
		run_js(js, "JSON.stringify(nymphTestValue(observed()))"),
		"[1,1]"
	);
}

#[test]
fn stdlib_sort_initializers_run_through_external_leaf_calls() {
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

// ── `return` inside an unbraced if/while branch ─────────────────────────────

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

// ── Call-site bound enforcement ─────────────────────────────────────────────
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

// ── String expressions ──────────────────────────────────────────────────────

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

// ── Range/for-loop expressions ──────────────────────────────────────────────

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
	// A source that itself implements `Iterator<Item>` is used
	// directly (`let $it = <src>`, no `.iter()` hop) — desugars to
	// `while ($go) { match ($it.next()) { Some(x) -> .., None -> $go = false } }`.
	// `Counter`'s `next` mutates `this.n`, which requires `this: mut Self` —
	// declaring `next` a `mut func` on both the interface (the source of
	// truth every gate reads) and this impl (whose restatement must
	// match) gets that without needing a `Mut`-self-type impl target (`impl ..
	// for mut Counter`), which lowering doesn't support yet for a class-backed
	// ADT, which lowering does not support for mutable self-type impl targets.
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
	// A source that implements `Iterable<T>` (not `Iterator` itself)
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

// ── `|>`, `in`/`!in`, `??` ─────────────────────────────────────────────────

#[test]
fn runs_pipe_chain_applies_functions_left_to_right() {
	// `|>` lowers structurally to a `Call`; chained pipes are left-associative, so
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
	// `a in c` / `a !in c` dispatch to `c.contains(a)` / `c.not_contains(a)` —
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
	// Nymph has no optional runtime representation, so `??`
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

// ── `namespace func` statics, `mut func` methods ───────────────────────────

#[test]
fn runs_struct_namespaced_static_called_from_nymph() {
	// `Type.func(args)` inside a Nymph body lowers structurally (a `Field`
	// callee, zero call-site changes) and the declaration lands as a JS
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
		js.matches("wrap(value, $type$0) {").count() + js.matches("wrap (value, $type$0) {").count(),
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
	// Lowering treats a `mut func` as an ordinary instance method after the
	// checker proves the caller has a mutable receiver. Emit supports the
	// resulting `this.field = ..` assignment target.
	let src = r#"
		struct Counter(n: int) {
			mut func bump(): void = { this.n = this.n + 1 }
		}
		func run_bump(c: mut Counter): int = {
			c.bump()
			c.n
		}
	"#;
	assert_eq!(run(src, "run_bump(new Counter({ n: new NInt(5) }))"), "6");
}

#[test]
fn runs_field_slot_reassignment_gated_on_a_mut_receiver() {
	// `mut T` is compile-time-only — codegen is a near
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
	// Field type authority is pinned end to end: `inner`'s own declared
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
	// `let mut` binds at `mut <ty(v)>` — reassigning that binding, and
	// passing it on to a plain (non-`mut`) parameter (`mut T <: T`, one-way),
	// both preserve ordinary immutable parameter behavior under Node.
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
	let src = r#"
		func set_unsigned(i: uint, v: int): #[int] = {
			let mut values = #[1, 2, 3]
			values[i] = v
			values
		}
	"#;
	assert_eq!(
		run(
			src,
			"JSON.stringify(nymphTestValue(set_unsigned(new NUint(1), new NInt(99))))"
		),
		"[1,99,3]"
	);
}

#[test]
fn runs_map_index_assignment() {
	// A JS `Map` has no assignment-expression
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

// ── `is`/`!is` desugar, `as` scalar/`Into` dispatch, end-to-end ────────────

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
	// takes `Math.abs` first so the cast is a real runtime operation.
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
	// `check_cast` must not hardcode the dispatched method name
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

// ── Namespaced static vs. interface-impl method ────────────────────────────

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

// ── Closures ─────────────────────────────────────────────────────────────────

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
	// emitted JS. A closure defined afterward, reading the
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

// ── Smart literal spread ────────────────────────────────────────────────────

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
	// Entries emit left-to-right in source order with no hoisting — a
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

#[test]
fn real_list_push_materializes_once_push_is_linked() {
	let user = r#"
		func check(): int = {
			let xs = #[1]
			xs.push(2)
			xs[1]
		}
	"#;
	assert_eq!(run(user, "check()"), "2");
}

#[test]
fn mixed_int_uint_operators_run_under_node() {
	// Mixed int/uint operators type-check
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

// `is_empty` (`this.length() == 0`) is real Nymph source, not `external`
// itself. Its transitive `length` call requires an external JS binding
// (`body_calls_unlinked_external`'s member-call extension caught it). Because
// `length` is a linked external (see
// `nymph_hir::linkage::REGISTRY`), `body_calls_unlinked_external` no
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

// `list.nym`'s `get` is `external(get)` and requires a
// loud defer for the identical reason `push` (above) still does: no JS
// binding anywhere for the marker. It is linked for a `List` receiver
// (`nymph_hir::linkage::REGISTRY`'s `("get", Some("list"))`/`("get",
// Some("mut_list"))` rows), so it
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
	let user = r#"
		func check(): int = match (#[7].get(0)) {
			Some(value) -> value,
			None -> 0,
		}
	"#;
	assert_eq!(run(user, "check()"), "7");
}

// `map.nym`'s `get` shares the same bare marker as
// `list.nym`'s (see
// `real_list_get_materializes_once_get_is_linked` above), but WAS a
// different, unlinked JS implementation — the registry's receiver-tag
// disambiguation (`Some("mut_map")`, since `map.nym` declares `get` inside
// its `impl<K,V> mut #{K:V}` block) lets a `Map` receiver's `get` materialize
// like `list`'s `get`, into `HirExpr::ExternCall` emitting a plain
// `get($_this, key)` call plus a deduped `import { get } from
// "std/collections/map"`. Shape-only (same reasoning as the list flips
// above) — the bundle-path e2e in `nymph-compiler`'s `tests/std_linkage.rs`
// proves the mechanism actually RUNS.
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

// `map.nym`'s `is_empty` (`this.size() == 0`) is
// transitively external through `size`, mirroring the list case above —
// `size` is a linked (unambiguous, `receiver_tag: None`) external, so
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
// on a NAMED enum receiver (`Option`/`Result`) materializes ONTO that
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
	// closure argument and itself constructs a `Some` via
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
	// exercising `declare`'s reserved-word rename too.
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
	// a later round because collecting `enum_name` off a
	// `VariantNew`, not just a bare `VariantRef` — matters here: `ok`/`err`'s
	// bodies never write a bare `None`/`Some` reference, only `Option.Some(..)`/
	// `Option.None` qualified constructions).
	//
	// Both checks call `is_some()`/pattern-match `Option` from WITHIN the
	// compiled Nymph program (not the JS driver text): `Option`'s `is_some`
	// method is itself only demand-materialized because a COMPILED call site
	// asks for it — a JS driver calling `.is_some()` directly would never
	// route through `try_materialize_prelude_dispatch` at all, so it
	// would not exercise this mechanism.
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
	// Compatibility lowering must preserve the receiverless generic dispatch
	// and its hidden ABI. End-to-end stable-project execution is covered by
	// `core_prelude_ambient::default_generic_bound_executes_through_the_ambient_canonical_type_object`.
	let user = r#"
		func get(o: Option<int>): int = o.map_or_default((x) -> x)
	"#;
	let js = compile(user);
	assert!(js.contains("$type$0.default()"), "{js}");
	assert!(js.contains("map_or_default(f, $type$0)"), "{js}");
	assert!(js.contains("o.map_or_default("), "{js}");
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

// ── Owned collection literal → `mut` coercion, driven under Node ────────────
//
// These use a self-contained synthetic setup — native `[]`
// index read/assign on `#{…}`/`#[…]`, which lowers to a plain JS `Map`/`Array`
// with no `external` linkage involved (`emit.rs`'s `HirExpr::Assign` arm) — to
// prove that a fresh collection literal accepted at a `mut`
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
	let user = "func take(xs: mut #[int]): int = {\n\txs[0u] = 99\n\txs[0]\n}\nfunc t(): int = take(#[1, 2, 3])";
	assert_eq!(run(user, "t()"), "99");
}

#[test]
fn a_fresh_map_literal_at_a_mut_struct_ctor_field_is_mutated_and_read_back() {
	let user = "struct Box(m: mut #{int: int}) {}\nfunc t(): int = {\n\tlet b = Box(m = #{1: 2})\n\tb.m[1] = 99\n\tb.m[1]\n}";
	assert_eq!(run(user, "t()"), "99");
}

// ── Unannotated if/block-bodied inherent method return type under Node ──────

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
	// A `mut func` may call another `mut func` on a mutable field projection;
	// the two `bump`s must mutate shared state (0 → 2).
	let user = "struct Inner(n: int) {\n\tmut func bump(): int = {\n\t\tthis.n = this.n + 1\n\t\tthis.n\n\t}\n}\nstruct Outer(inner: Inner) {\n\tmut func step(): int = this.inner.bump()\n}\nfunc t(): int = {\n\tlet mut o = Outer(inner = Inner(n = 0))\n\tlet a = o.step()\n\tlet b = o.step()\n\ta + b\n}";
	assert_eq!(run(user, "t()"), "3");
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
fn labeled_loops_and_callable_returns_execute() {
	let src = r#"func outer_break(): int = match (while@outer (true) {
  while (true) { break@outer 7 }
}) { Some(value) -> value, None -> 0 }
func outer_continue(): int = {
  let mut count = 0
  while@outer (count < 3) {
    count = count + 1
    while (true) { continue@outer }
  }
  count
}
func for_break(): int = match (for@outer (value in 1..4) {
  while (true) { break@outer value }
}) { Some(value) -> value, None -> 0 }
func direct_break(): int = match (while@outer (true) { break 8 }) {
  Some(value) -> value,
  None -> 0,
}
func direct_for_break(): int = match (for@outer (value in 1..4) { break value }) {
  Some(value) -> value,
  None -> 0,
}
func named(): int = { return@named 9 }
func closure(): int = { let f = done@() -> { return@done 11 } f() }
func body_closure(): int = { let f = () -> done@{ return@done 12 } f() }
func dual_closure(): int = { let f = done@() -> done@{ return@done 13 } f() }"#;
	assert_eq!(run(src, "outer_break()"), "7");
	assert_eq!(run(src, "outer_continue()"), "3");
	assert_eq!(run(src, "for_break()"), "1");
	assert_eq!(run(src, "direct_break()"), "8");
	assert_eq!(run(src, "direct_for_break()"), "1");
	assert_eq!(run(src, "named()"), "9");
	assert_eq!(run(src, "closure()"), "11");
	assert_eq!(run(src, "body_closure()"), "12");
	assert_eq!(run(src, "dual_closure()"), "13");
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
fn question_propagation_executes_for_option_result_and_labeled_blocks() {
	let src = r#"struct Counter(calls: int) {}
impl Counter {
  mut func option(present: boolean): Option<int> = {
    this.calls = this.calls + 1
    if (present) Some(4) else None
  }
}
func option(present: boolean): Option<string> = {
  let mut counter = Counter(calls = 0)
  let value = counter.option(present)?
  Some("${value}:${counter.calls}")
}
func result(ok: boolean): Result<string, int> = {
  let value = (if (ok) Ok(5) else Error(9))?
  Ok("${value}")
}
func labeled(present: boolean): Option<string> = target@{
  let value = (if (present) Some(6) else None)?@target
  Some("${value}")
}
func nearest(present: boolean): int = {
  let inner: () -> Option<string> = () -> {
    let value = (if (present) Some(7) else None)?
    Some("${value}")
  }
  inner()
  8
}"#;
	assert_eq!(run(src, "option(new NBool(true))"), "{ value: '4:1' }");
	assert_eq!(run(src, "option(new NBool(false))"), "{}");
	assert_eq!(run(src, "result(new NBool(true))"), "{ value: '5' }");
	assert_eq!(run(src, "result(new NBool(false))"), "{ error: 9 }");
	assert_eq!(run(src, "labeled(new NBool(true))"), "{ value: '6' }");
	assert_eq!(run(src, "labeled(new NBool(false))"), "{}");
	assert_eq!(run(src, "nearest(new NBool(false))"), "8");
}

#[test]
fn explicitly_outer_breaks_inside_nested_break_values_count_and_execute() {
	let src = r#"func nested(): int = match (while@outer (true) {
	while (true) { break (break@outer 7) }
}) { Some(value) -> value, None -> 0 }"#;
	assert_eq!(run(src, "nested()"), "7");
}

#[test]
fn completion_packets_cannot_shadow_user_bindings_or_inspect_user_exceptions() {
	let src = r#"func preserve(_t1: int): int = {
  let value = block@{ return@block _t1 }
  value
}
func invoke(callback: () -> int): int = {
  let value = block@{
    callback()
    if (true) { return@invoke 1 }
    2
  }
  value
}
func loop_invoke(callback: () -> int): Option<int> = while (true) {
  callback()
  break 1
}"#;
	assert_eq!(run(src, "preserve(new NInt(7))"), "7");
	assert_eq!(
		run(
			src,
			"(() => { const sentinel = new Proxy([], { get() { throw new Error('inspected') } }); try { invoke(() => { throw sentinel }) } catch (error) { return error === sentinel } return false })()",
		),
		"true"
	);
	assert_eq!(
		run(
			src,
			"(() => { const sentinel = new Proxy([], { get() { throw new Error('inspected') } }); try { loop_invoke(() => { throw sentinel }) } catch (error) { return error === sentinel } return false })()",
		),
		"true"
	);
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
  let mut result = static_seed(Box(value = 0))
  result = result + static_seed(Box(value = "")) * 10
  result = result + instance_seed(Box(value = 0)) * 100
  result = result + instance_seed(Box(value = "")) * 1000
  result = result + static_seed(Token.Value(value = 0)) * 10000
  result = result + instance_seed(Token.Value(value = "")) * 100000
  result = result + instance_seed(int_empty()) * 1000000
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
		js.contains("function $m0$impl$i1$seed($self, $type$0)"),
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
		js.contains("combine($self, new NBool(true), $type$0, NBool.prototype)"),
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
		js.contains("$member, NInt.prototype)"),
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
		js.contains("return $m0$impl$i0$seed($receiver, $arg0, NInt.prototype);"),
		"method-value adapter must preserve receiver, source, hidden ABI order: {js}"
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
		js.contains("$m0$impl$i1$apply($receiver, $arg0, $arg1, NInt.prototype, NString.prototype)"),
		"method hidden object must follow receiver, source, and implementation slots: {js}"
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
fn hidden_type_objects_preserve_source_argument_order_and_exactly_once_evaluation() {
	let source = r#"
interface Seed { func seed(value: int): int }
impl Seed for int { func seed(value: int) = value }

func direct<T: Seed>(marker: T, value: int): int = T.seed(value)
func answer(): int = {
  let mut trace = 0
  let marker = () -> { trace = trace * 10 + 1
    0 }
  let value = () -> { trace = trace * 10 + 2
    40 }
  direct(marker(), value()) + trace
}
"#;
	assert_eq!(run(source, "answer()"), "52");
}
