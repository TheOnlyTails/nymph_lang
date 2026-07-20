//! Golden regression corpus: realistic known-good Nymph programs pinned so
//! future slices cannot silently regress them.
//!
//! Two tiers:
//! - **Compile-clean tier**: each program must compile with zero diagnostics and
//!   without panicking (a lowering panic here means a slice regressed a feature
//!   that used to work).
//! - **Run tier**: the emitted JS also executes under `node`, asserting stdout.
//!
//! The corpus deliberately stays inside the implemented surface (Slices 0–4H).
//! Known deferrals it must NOT touch: closures (incl. calling a function
//! *value*/function-typed parameter — the type-checks but has no HIR lowering
//! path), range *expressions* in general value position (range patterns and
//! `for`-loop iterable ranges are fine; non-numeric/char ranges and
//! `To`/`ToInclusive`/`From` range sources in `for` still panic), `as` casts,
//! `?`/`!` postfix, `??`/`in`/`!in`/`|>`, user `==`/`!=` dispatch (native `===`
//! always, by design per 4C-c — not a missing-feature deferral), namespaced/
//! static methods, mut methods, positional (unlabeled) variant/struct
//! construction, zero-field structs, blanket-impl materialization, stdlib
//! imports, interface-impl method own-generics (4G-b, inexpressible today).
//! Enum methods/impls (4D) and string literal expressions (4H) are now
//! IMPLEMENTED — do not re-add them here.
//!
//! Sharpest deferral to remember: a bounded-generic *operator* used via
//! operator SYNTAX (bare `a < b` where `a: T, T: Comparable<Other = T>`)
//! still panics in lowering (`DispatchKind::UserImplDefaultMethod` — an honest
//! deferral, not a silent miscompile). A bounded-generic *method call* through
//! the same bound (`a.less_than(b)`, or `a.area()` under `T: Area`) works fine
//! today, because plain method calls lower to `Call{Field{..}}` regardless of
//! dispatch kind — only literal operator syntax needs the (still-missing)
//! dispatch table entry. Golden programs exercising comparisons on a raw
//! generic Param must use the explicit method call, never the operator.
//!
//! Parse gotchas honored throughout: `if`/`while`/`for` require parens around
//! their whole header (`for (x in a..b) { .. }`); match arms use `->` and
//! commas; a guard whose expression ends in an identifier must be
//! parenthesized (otherwise `ident -> body` parses as a closure); line-leading
//! operators continue the previous expression; a field-variant pattern binds
//! under the FIELD's own name unless aliased via `field = name` (`Some(n2)`
//! against a variant whose field is named `n` is "unknown field `n2`", not a
//! fresh binding — write `Some(n = n2)` to bind under a different name).

use std::io::Write;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

/// Compile a program, asserting zero diagnostics, and return the emitted JS.
fn compile_ok(src: &str) -> String {
	match nymph_compiler::compile(src, "golden") {
		Ok(js) => js,
		Err(diags) => panic!("expected a clean compile, got diagnostics: {diags:?}\n---\n{src}"),
	}
}

/// Compile `src`, append a driver that logs `call`, run under Node, return
/// trimmed stdout. Local copy of the `run_node.rs` helper pattern (tests may
/// not import from another crate's test files).
fn run(src: &str, call: &str) -> String {
	let mut js = compile_ok(src);
	js.push_str(&format!("\nconsole.log({call});\n"));

	// All tests in this binary share one process and may run on parallel
	// threads; mix a counter into the temp filename to avoid races.
	static COUNTER: AtomicU64 = AtomicU64::new(0);
	let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
	let dir = std::env::temp_dir();
	let path = dir.join(format!("nymph_golden_{}_{unique}.mjs", std::process::id()));
	let mut file = std::fs::File::create(&path).unwrap();
	file.write_all(js.as_bytes()).unwrap();

	// The shell may force ANSI color (`FORCE_COLOR`), corrupting stdout asserts.
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

// ═══════════════════════════════════════════════════════════════════════════
// Tier 1: compile-clean programs
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn golden_arithmetic_blocks_and_mutation() {
	// Every arithmetic operator, precedence, blocks as expressions, `let mut`,
	// plain and compound assignment, and a while accumulator — combined the way
	// a real numeric routine would be written.
	compile_ok(
		r#"
		func polynomial(x: int): int = x ** 3 + 2 * x ** 2 - x + 7

		func average_scaled(a: int, b: int, scale: int): int = {
			let sum = a + b
			let scaled = sum * scale
			scaled / 2
		}

		func accumulate(n: int): int = {
			let mut total = 0
			let mut i = 1
			while (i <= n) {
				total += i * i
				i += 1
			}
			total % 1000
		}

		func spread(lo: int, hi: int): int = {
			let mut range = hi - lo
			range *= 2
			range -= 1
			range /= 3
			range
		}
		"#,
	);
}

#[test]
fn golden_comparisons_logical_ops_and_branching() {
	// Comparison operators on ints and floats, boolean algebra with
	// short-circuiting, and if/else chains in value position.
	compile_ok(
		r#"
		func in_band(x: float, lo: float, hi: float): boolean = lo <= x && x <= hi

		func outside(x: float, lo: float, hi: float): boolean = !in_band(x, lo, hi)

		func clamp(x: int, lo: int, hi: int): int =
			if (x < lo) { lo }
			else { if (x > hi) { hi } else { x } }

		func same_sign(a: int, b: int): boolean =
			a > 0 && b > 0 || a < 0 && b < 0 || a == 0 && b == 0

		func any_zero(a: int, b: int, c: int): boolean = a == 0 || b == 0 || c == 0
		"#,
	);
}

#[test]
fn golden_bit_manipulation() {
	// The full bitwise operator set, including unary `~` and shift chains.
	compile_ok(
		r#"
		func set_flag(mask: int, flag: int): int = mask | flag
		func clear_flag(mask: int, flag: int): int = mask & ~flag
		func toggle(mask: int, flag: int): int = mask ^ flag
		func low_byte(word: int): int = word & 255
		func swap_nibbles(b: int): int = (b & 15) << 4 | (b >> 4) & 15

		func popcount_ish(x: int): int = {
			let mut n = x
			let mut count = 0
			while (n != 0) {
				count += n & 1
				n >>= 1
			}
			count
		}
		"#,
	);
}

#[test]
fn golden_int_literal_widening_and_uint() {
	// Int literals widen to float/uint in return, argument, comparison, and
	// operand positions; uint arithmetic stays well-typed.
	compile_ok(
		r#"
		func half(): float = 1 / 2.0
		func to_float_default(): float = 3
		func takes_float(x: float): float = x * 2
		func widened_arg(): float = takes_float(7)
		func positive(x: float): boolean = x > 0
		func count(): uint = 42u
		func bump_uint(n: uint): uint = n + 1
		func is_empty(n: uint): boolean = n == 0
		"#,
	);
}

#[test]
fn golden_char_comparisons_and_match() {
	// Chars as parameters, char equality, and a match over char literals.
	compile_ok(
		r#"
		func is_newline(c: char): boolean = c == '\n'

		func vowel_index(c: char): int = match (c) {
			'a' -> 1,
			'e' -> 2,
			'i' -> 3,
			'o' -> 4,
			'u' -> 5,
			_ -> 0,
		}

		func shift_class(c: char): int = if (c == 'z') { -1 } else { vowel_index(c) }
		"#,
	);
}

#[test]
fn golden_lists_indexing_and_nesting() {
	// List literals (including nested lists and lists of expressions),
	// indexing, and index chains.
	compile_ok(
		r#"
		func first_of(xs: #[int]): int = xs[0]

		func corner(grid: #[#[int]]): int = grid[0][0]

		func built(): int = {
			let row = #[1, 2, 3]
			let grid = #[row, #[4, 5, 6]]
			grid[1][2] + first_of(row)
		}

		func weighted(xs: #[int], w: int): int = xs[0] * w + xs[1]
		"#,
	);
}

#[test]
fn golden_tuples_and_maps() {
	// Tuple construction and constant-index access; map literals with int keys
	// and map indexing (string keys need string exprs — deferred).
	compile_ok(
		r#"
		func swap_sum(t: #(int, int)): int = t[1] + t[0]

		func pair_up(a: int, b: boolean): #(int, boolean) = #(a, b)

		func score_table(): #{int: int} = #{ 1: 100, 2: 250, 3: 500 }

		func lookup(level: int): int = score_table()[level]

		func mixed(): int = {
			let point = #(3, 4)
			let bonus = #{ 0: 10, 1: 20 }
			point[0] + bonus[1]
		}
		"#,
	);
}

#[test]
fn golden_functions_and_generics() {
	// Generic identity specialised at several types, generic pairing through a
	// generic struct, and call chains between module functions.
	compile_ok(
		r#"
		struct Pair<A, B>(first: A, second: B)

		func id<T>(x: T): T = x

		func both<A, B>(a: A, b: B): Pair<A, B> = Pair(first = a, second = b)

		func use_int(): int = id(5)
		func use_bool(): boolean = id(true)
		func use_pair(): int = both(1, true).first
		func chained(): int = id(id(7)) + use_int()
		"#,
	);
}

#[test]
fn golden_generic_bound_method_dispatch() {
	// A generic function whose bound provides a method, called with a concrete
	// impl; plus the `impl Trait`-parameter sugar for the same thing.
	compile_ok(
		r#"
		interface Area { func area(): int }

		struct Square(side: int)
		impl Area for Square {
			func area(): int = this.side * this.side
		}

		struct Rect(w: int, h: int)
		impl Area for Rect {
			func area(): int = this.w * this.h
		}

		func measure<T: Area>(shape: T): int = shape.area()

		// `impl Trait`-parameter sugar: the bound resolves `shape.area()` inside the
		// body. (Calling `measure_sugar` with a concrete argument is a separate,
		// currently-broken story — see the finding test at the bottom of this file.)
		func measure_sugar(shape: Area): int = shape.area()

		func total(s: Square, r: Rect): int = measure(s) + measure(r)
		"#,
	);
}

#[test]
fn golden_structs_construction_and_nesting() {
	// Struct-in-struct nesting, labeled construction, field chains, and a list
	// of struct values.
	compile_ok(
		r#"
		struct Point(x: int, y: int)
		struct Segment(from: Point, to: Point)

		func length_sq(s: Segment): int = {
			let dx = s.to.x - s.from.x
			let dy = s.to.y - s.from.y
			dx * dx + dy * dy
		}

		func origin_segment(p: Point): Segment =
			Segment(from = Point(x = 0, y = 0), to = p)

		func poly_first_x(): int = {
			let points = #[Point(x = 1, y = 1), Point(x = 2, y = 4)]
			points[0].x
		}
		"#,
	);
}

#[test]
fn golden_struct_methods_inherent() {
	// Inherent methods via a top-level `impl` block AND declared inside the
	// struct body; `this`, sibling method calls, method params, and control flow
	// in method bodies.
	compile_ok(
		r#"
		struct Account(balance: int, overdraft: int) {
			func available(): int = this.balance + this.overdraft
		}

		impl Account {
			func can_spend(amount: int): boolean = amount <= this.available()
			func after_spend(amount: int): int =
				if (this.can_spend(amount)) { this.balance - amount }
				else { this.balance }
		}

		func demo(a: Account): int = a.after_spend(50)
		"#,
	);
}

#[test]
fn golden_generic_struct_with_methods() {
	// A generic container struct with an `impl<T>` block whose methods return
	// and consume the type parameter.
	compile_ok(
		r#"
		struct Slot<T>(value: T, occupied: boolean)

		impl<T> Slot<T> {
			func get(): T = this.value
			func is_free(): boolean = !this.occupied
		}

		func read_int(s: Slot<int>): int = if (s.is_free()) { 0 } else { s.get() }
		func read_flag(s: Slot<boolean>): boolean = s.get()
		"#,
	);
}

#[test]
fn golden_operator_overloading_binary() {
	// Binary operator overloads through both the nested-impl and top-level
	// impl-for forms, with chained operator expressions; the arithmetic inside
	// the method bodies stays native.
	compile_ok(
		r#"
		interface MyPlus<Other, Output> { func plus(other: Other): Output }
		interface MyMinus<Other, Output> { func minus(other: Other): Output }
		interface MyTimes<Other, Output> { func times(other: Other): Output }

		struct Vec2(x: int, y: int) {
			impl MyPlus<Other = Vec2, Output = Vec2> {
				func plus(other: Vec2): Vec2 = Vec2(x = this.x + other.x, y = this.y + other.y)
			}
		}

		impl MyMinus<Other = Vec2, Output = Vec2> for Vec2 {
			func minus(other: Vec2): Vec2 = Vec2(x = this.x - other.x, y = this.y - other.y)
		}

		impl MyTimes<Other = int, Output = Vec2> for Vec2 {
			func times(scale: int): Vec2 = Vec2(x = this.x * scale, y = this.y * scale)
		}

		func lerpish(a: Vec2, b: Vec2): Vec2 = a + (b - a) * 2
		"#,
	);
}

#[test]
fn golden_operator_overloading_unary() {
	// Unary `-`, `!`, and `~` overloads on user types (Slice 4C-a), alongside
	// native unary on primitives in the same program.
	compile_ok(
		r#"
		interface MyNegate<Output> { func negate(): Output }
		interface MyNot<Output> { func not(): Output }
		interface MyBitNot<Output> { func bit_not(): Output }

		struct Vec2(x: int, y: int)
		impl MyNegate<Output = Vec2> for Vec2 {
			func negate(): Vec2 = Vec2(x = -this.x, y = -this.y)
		}

		struct Tristate(known: boolean, value: boolean)
		impl MyNot<Output = Tristate> for Tristate {
			func not(): Tristate = Tristate(known = this.known, value = !this.value)
		}

		struct Mask(bits: int)
		impl MyBitNot<Output = Mask> for Mask {
			func bit_not(): Mask = Mask(bits = ~this.bits)
		}

		func flip_all(v: Vec2, t: Tristate, m: Mask): int = {
			let nv = -v
			let nt = !t
			let nm = ~m
			nv.x + nm.bits + if (nt.value) { 1 } else { 0 }
		}
		"#,
	);
}

#[test]
fn golden_operator_compound_assign_on_user_type() {
	// `+=` on a user type dispatches through the recorded resolution (the 4B
	// closeout's critical fix) — pinned here at the whole-program level.
	compile_ok(
		r#"
		interface MyPlus<Other, Output> { func plus(other: Other): Output }

		struct Money(cents: int)
		impl MyPlus<Other = Money, Output = Money> for Money {
			func plus(other: Money): Money = Money(cents = this.cents + other.cents)
		}

		func total(prices: #[Money]): Money = {
			let mut sum = Money(cents = 0)
			let mut i = 0
			while (i < 3) {
				sum += prices[i]
				i += 1
			}
			sum
		}
		"#,
	);
}

#[test]
fn golden_interface_default_methods() {
	// Interface default-method materialization (Slice 4C-b): dispatch through
	// the `<` operator, an explicit call to a default, and an override winning.
	compile_ok(
		r#"
		interface MyComparable<Other> {
			func compare_to(other: Other): int
			func less_than(other: Other): boolean = this.compare_to(other) < 0
		}

		struct Version(major: int, minor: int)
		impl MyComparable<Other = Version> for Version {
			func compare_to(other: Version): int =
				if (this.major != other.major) { this.major - other.major }
				else { this.minor - other.minor }
		}

		struct Priority(level: int)
		impl MyComparable<Other = Priority> for Priority {
			func compare_to(other: Priority): int = this.level - other.level
			func less_than(other: Priority): boolean = this.level < other.level
		}

		func needs_upgrade(installed: Version, latest: Version): boolean = installed < latest
		func explicit(installed: Version, latest: Version): boolean = installed.less_than(latest)
		func more_urgent(a: Priority, b: Priority): boolean = a < b
		"#,
	);
}

#[test]
fn golden_enums_construction_and_qualification() {
	// Nullary and field variants, generic enums, bare and qualified
	// construction, and variant names shared across enums. `Option` is the
	// ambient core one (no local declaration) — core-name redefinition was
	// made a diagnosed collision once core became the ambient prelude.
	compile_ok(
		r#"
		enum Status { Active, Suspended(reason_code: int), Closed }
		enum Tree { Leaf(v: int), Branch }
		enum Plant { Leaf(v: int), Root }

		func fresh(): Status = Active
		func banned(code: int): Status = Suspended(reason_code = code)
		func done(): Status = Status.Closed

		func wrap(n: int): Option<int> = Some(value = n)
		func nothing(): Option<int> = Option.None

		func tree_leaf(): Tree = Tree.Leaf(v = 1)
		func plant_leaf(): Plant = Plant.Leaf(v = 2)
		"#,
	);
}

#[test]
fn golden_match_variants_bindings_and_guards() {
	// Match over enums: payload bindings, nested variant patterns, guards
	// (parenthesized — a guard ending in an identifier would otherwise eat the
	// `->` as a closure), and wildcard fallthrough. `Option` is the ambient
	// core one (no local declaration) — see
	// `golden_enums_construction_and_qualification`.
	compile_ok(
		r#"
		enum Request { Get(id: int), Put(id: int, payload: int), Ping }

		func unwrap_or(o: Option<int>, fallback: int): int = match (o) {
			Some(value) -> value,
			None -> fallback,
		}

		func flatten(oo: Option<Option<int>>): int = match (oo) {
			Some(value = Some(value)) -> value,
			Some(value = None) -> -1,
			None -> -2,
		}

		func route(r: Request, limit: int): int = match (r) {
			Get(id) if (id > limit) -> -1,
			Get(id) -> id,
			Put(id, payload) if (payload == 0) -> id,
			Put(id, payload) -> id + payload,
			Ping -> 0,
		}
		"#,
	);
}

#[test]
fn golden_match_scalars_ranges_and_bindings() {
	// Scalar matches: literal arms, range patterns (exclusive and inclusive),
	// bindings with guards, and wildcard.
	compile_ok(
		r#"
		func http_class(code: int): int = match (code) {
			200 -> 1,
			301 | 302 -> 2,
			400..500 -> 3,
			500..=599 -> 4,
			n if (n < 100) -> -1,
			_ -> 0,
		}

		func bool_name_len(b: boolean): int = match (b) {
			true -> 4,
			false -> 5,
		}
		"#,
	);
}

#[test]
fn golden_match_structural_patterns() {
	// Structural patterns: tuple, struct, list (exact, rest, rest-with-suffix),
	// map, and union patterns — the 3B surface.
	compile_ok(
		r#"
		struct Point(x: int, y: int)
		enum Color { Red, Green, Blue }

		func quadrantish(p: #(int, int)): int = match (p) {
			#(0, 0) -> 0,
			#(x, y) if (x > 0 && y > 0) -> 1,
			#(x, _) if (x < 0) -> 2,
			_ -> 3,
		}

		func on_axis(pt: Point): boolean = match (pt) {
			Point(x = 0, y = _) -> true,
			Point(x = _, y = 0) -> true,
			_ -> false,
		}

		func describe(xs: #[int]): int = match (xs) {
			#[] -> 0,
			#[only] -> only,
			#[first, ...mid, last] -> first + last,
			_ -> -1,
		}

		func config_level(m: #{int: int}) : int = match (m) {
			#{ 1: v } -> v,
			_ -> -1,
		}

		func is_warm(c: Color): boolean = match (c) {
			Red | Green -> true,
			Blue -> false,
		}
		"#,
	);
}

#[test]
fn golden_late_resolved_inference() {
	// `let xs = #[]` whose element type is pinned only later — both by a plain
	// annotation and through an operator that must re-resolve once pinned
	// (the 4C-c pending-operator queue).
	compile_ok(
		r#"
		interface MyComparable<Other> { func less_than(other: Other): boolean }
		struct Card(rank: int)
		impl MyComparable<Other = Card> for Card {
			func less_than(other: Card): boolean = this.rank < other.rank
		}

		func pinned_by_annotation(): int = {
			let xs = #[]
			let first = xs[0]
			let pin: #[int] = xs
			first
		}

		func pinned_through_operator(a: Card, b: Card): boolean = {
			let xs = #[a, b]
			let lt = xs[0] < xs[1]
			let pin: #[Card] = xs
			lt
		}
		"#,
	);
}

#[test]
fn golden_recursion_direct_and_mutual() {
	// Direct and mutual recursion between module functions.
	compile_ok(
		r#"
		func fact(n: int): int = if (n <= 1) { 1 } else { n * fact(n - 1) }

		func fib(n: int): int =
			if (n < 2) { n } else { fib(n - 1) + fib(n - 2) }

		func is_even(n: int): boolean = if (n == 0) { true } else { is_odd(n - 1) }
		func is_odd(n: int): boolean = if (n == 0) { false } else { is_even(n - 1) }
		"#,
	);
}

#[test]
fn golden_control_flow_as_expressions() {
	// if/else and match in value position (operands, arguments, let
	// initializers), a statement-position if without else, and nested whiles.
	compile_ok(
		r#"
		func pick(cond: boolean, a: int, b: int): int = if (cond) { a } else { b }

		func nested_value_if(n: int): int =
			1 + if (n > 0) { pick(n > 10, 100, 10) } else { 0 }

		func grid_sum(w: int, h: int): int = {
			let mut total = 0
			let mut y = 0
			while (y < h) {
				let mut x = 0
				while (x < w) {
					total += x * y
					x += 1
				}
				y += 1
			}
			total
		}

		func statement_if(n: int): int = {
			let mut x = 0
			if (n > 0) { x = n }
			x
		}

		func match_operand(n: int): int = 10 * match (n) { 0 -> 1, _ -> 2 }
		"#,
	);
}

#[test]
fn golden_program_geometry() {
	// A realistic program combining most of the surface at once: structs with
	// methods and operator impls, an enum matched with bindings and guards, a
	// generic bounded function, collections, and inference.
	compile_ok(
		r#"
		interface MyPlus<Other, Output> { func plus(other: Other): Output }
		interface Area { func area(): int }

		struct Vec2(x: int, y: int) {
			impl MyPlus<Other = Vec2, Output = Vec2> {
				func plus(other: Vec2): Vec2 = Vec2(x = this.x + other.x, y = this.y + other.y)
			}
			func dot(other: Vec2): int = this.x * other.x + this.y * other.y
		}

		enum Shape { Circle(r: int), Rectangle(w: int, h: int), Dot }

		struct Sprite(pos: Vec2, shape: Shape)

		impl Area for Sprite {
			func area(): int = match (this.shape) {
				Circle(r) -> 3 * r * r,
				Rectangle(w, h) if (w == h) -> w * w,
				Rectangle(w, h) -> w * h,
				Dot -> 0,
			}
		}

		func biggest<T: Area>(a: T, b: T): int = {
			let first = a.area()
			let second = b.area()
			if (first > second) { first } else { second }
		}

		func scene(): int = {
			let a = Sprite(pos = Vec2(x = 0, y = 0), shape = Circle(r = 2))
			let b = Sprite(pos = Vec2(x = 3, y = 4), shape = Rectangle(w = 5, h = 5))
			let moved = a.pos + b.pos
			biggest(a, b) + moved.dot(b.pos)
		}
		"#,
	);
}

#[test]
fn golden_program_inventory() {
	// Another combined program: maps and lists driving business-ish logic,
	// compound assignment, enum results, and while loops over indices.
	// `Full` (not `Ok`) — a core-name (`Result.Ok`) clash was made a
	// diagnosed collision once core became the ambient prelude.
	compile_ok(
		r#"
		enum Verdict { Full, Short(missing: int) }

		struct Item(sku: int, count: int) {
			func short_by(needed: int): int =
				if (this.count >= needed) { 0 } else { needed - this.count }
		}

		func check(items: #[Item], needed: int): Verdict = {
			let mut missing = 0
			let mut i = 0
			while (i < 2) {
				missing += items[i].short_by(needed)
				i += 1
			}
			if (missing == 0) { Full } else { Short(missing = missing) }
		}

		func penalty(v: Verdict): int = match (v) {
			Full -> 0,
			Short(missing) if (missing > 10) -> missing * 2,
			Short(missing) -> missing,
		}

		func demo(): int = {
			let stock = #[Item(sku = 1, count = 3), Item(sku = 2, count = 9)]
			penalty(check(stock, 5))
		}
		"#,
	);
}

#[test]
fn golden_program_state_machine() {
	// An enum-driven state machine: match computes the next state, a while loop
	// steps it, and the result is compared against variants.
	compile_ok(
		r#"
		enum Light { Red, Yellow, Green }

		func next(l: Light): Light = match (l) {
			Red -> Green,
			Green -> Yellow,
			Yellow -> Red,
		}

		func step_n(start: Light, n: int): Light = {
			let mut state = start
			let mut i = 0
			while (i < n) {
				state = next(state)
				i += 1
			}
			state
		}

		func is_stop_after(n: int): boolean = match (step_n(Red, n)) {
			Red -> true,
			_ -> false,
		}
		"#,
	);
}

#[test]
fn golden_shared_method_names_across_types() {
	// The same method name defined on different types (inherent and via
	// distinct interfaces) resolves per receiver type without ambiguity.
	compile_ok(
		r#"
		interface Scored { func score(): int }

		struct Player(points: int)
		impl Scored for Player { func score(): int = this.points }

		struct Team(total: int, bonus: int)
		impl Scored for Team { func score(): int = this.total + this.bonus }

		struct Judge(bias: int) {
			func score(): int = this.bias
		}

		func tally(p: Player, t: Team, j: Judge): int = p.score() + t.score() + j.score()
		"#,
	);
}

#[test]
fn golden_float_arithmetic_and_division() {
	// Float-only arithmetic, mixed literal widening, remainder, and unary
	// negate on floats.
	compile_ok(
		r#"
		func mean(a: float, b: float): float = (a + b) / 2

		func fractional(x: float): float = x % 1.0

		func negated_mean(a: float, b: float): float = -mean(a, b)

		func compound_scale(x: float): float = {
			let mut v = x
			v *= 1.5
			v /= 2
			v += 0.25
			v
		}
		"#,
	);
}

#[test]
fn golden_enum_payload_struct_roundtrip() {
	// Struct values inside enum payloads, matched back out with nested struct
	// patterns inside variant patterns.
	compile_ok(
		r#"
		struct Point(x: int, y: int)
		enum Event { Click(at: Point), Scroll(delta: int), Idle }

		func x_of(e: Event): int = match (e) {
			Click(at = Point(x = px, y = _)) -> px,
			Scroll(delta) -> delta,
			Idle -> 0,
		}

		func mk_click(x: int, y: int): Event = Click(at = Point(x = x, y = y))
		"#,
	);
}

#[test]
fn golden_enum_inherent_methods_read_payload_via_match_this() {
	// Enum methods (Slice 4D): `this.field` is rejected on enum receivers, so
	// an inherent method reads its payload by matching `this` itself.
	compile_ok(
		r#"
		enum Shape { Circle(r: int), Square(s: int) }
		impl Shape {
			func area(): int = match (this) {
				Circle(r) -> 3 * r * r,
				Square(s) -> s * s,
			}
		}
		func total(a: Shape, b: Shape): int = a.area() + b.area()
		"#,
	);
}

#[test]
fn golden_enum_operator_impl() {
	// A binary operator impl on an enum (Slice 4D + 4B), matching `this` and
	// `other` to build a new variant. Also exercises the `field = name` pattern
	// alias, needed here because the inner match would otherwise re-bind the
	// same field name at both levels.
	compile_ok(
		r#"
		interface MyPlus<Other, Output> { func plus(other: Other): Output }
		enum Count { Zero, Val(n: int) }
		impl MyPlus<Other = Count, Output = Count> for Count {
			func plus(other: Count): Count = match (this) {
				Zero -> other,
				Val(n = a) -> match (other) {
					Zero -> Val(n = a),
					Val(n = b) -> Val(n = a + b),
				},
			}
		}
		func combine(a: Count, b: Count): Count = a + b
		"#,
	);
}

#[test]
fn golden_enum_interface_default_method() {
	// An interface default method (4C-b) materialized on an enum impl (4D):
	// `doubled_label` is defined only in the interface, dispatched through an
	// enum receiver.
	compile_ok(
		r#"
		interface Describable {
			func label(): int
			func doubled_label(): int = this.label() * 2
		}
		enum Level { Low, High }
		impl Describable for Level {
			func label(): int = match (this) {
				Low -> 1,
				High -> 2,
			}
		}
		func demo(l: Level): int = l.doubled_label()
		"#,
	);
}

#[test]
fn golden_enum_methodless_alongside_methodful() {
	// A method-less enum (byte-identical emit, no proto) used alongside a
	// methodful one (proto-based ABI) in the same program — the two ABIs must
	// coexist without interfering.
	compile_ok(
		r#"
		enum Plain { A, B }
		enum Shape { Circle(r: int) }
		impl Shape {
			func area(): int = match (this) { Circle(r) -> 3 * r * r }
		}
		func classify(p: Plain): int = match (p) { A -> 0, B -> 1 }
		func demo(p: Plain, s: Shape): int = classify(p) + s.area()
		"#,
	);
}

#[test]
fn golden_bounded_generic_comparable_via_bound_explicit_call() {
	// Comparisons on generics (4C-c): a bounded generic function body calls
	// a bound-provided method THROUGH the bound. Deliberately an explicit
	// method call, not `<` operator syntax — see the file header's
	// sharpest-deferral note. The method is named `lighter_than`, not
	// `less_than`: with the stdlib operator prelude on by default, a method
	// named `less_than` would resolve through the prelude's own blanket
	// `Comparable` impl (a genuine resolution-precedence surprise reported
	// separately, not patched around here — KK5) instead of this test's own
	// user-declared bound.
	compile_ok(
		r#"
		interface MyComparable<Other> { func lighter_than(other: Other): boolean }
		struct Weight(kg: int)
		impl MyComparable<Other = Weight> for Weight {
			func lighter_than(other: Weight): boolean = this.kg < other.kg
		}
		func lighter<T: MyComparable<Other = T>>(a: T, b: T): T =
			if (a.lighter_than(b)) { a } else { b }
		func demo(x: Weight, y: Weight): Weight = lighter(x, y)
		"#,
	);
}

// KNOWN BUG (reported, not fixed here — needs its own designed slice): when a
// user's generic bound declares a method whose NAME collides with a method a
// stdlib blanket impl provides (e.g. a bound `MyComparable` with `less_than`,
// colliding with the prelude's blanket `Comparable`), `resolve_method` on a
// still-generic `Param` receiver resolves through the blanket impl instead of
// the parameter's own declared bound (`head_of` returns `None` for a bare
// `Param`, so phase 1's candidate search only ever finds blanket buckets).
// The sibling test above sidesteps it by renaming the method to
// `lighter_than`. A naive fix — reordering `resolve_param_method` ahead of
// phase 1 — was tried and REVERTED: it breaks explicit method calls through a
// truly-unconstrained blanket impl (`func same<T>(a: T, b: T) = a.equals(b)`
// via the prelude's blanket `Equals` stops resolving), which is the exact
// shape the default prelude must keep working. The real fix needs designed
// precedence rules between declared bounds and blanket impls.

#[test]
fn golden_late_pinned_comparison_via_param_annotation() {
	// A second late-pinning site for the 4C-c queue, distinct from the
	// `let`-annotation one above: the empty list's element type is pinned by
	// flowing into a FUNCTION PARAMETER's annotated type instead.
	compile_ok(
		r#"
		interface MyComparable<Other> { func less_than(other: Other): boolean }
		struct Card(rank: int)
		impl MyComparable<Other = Card> for Card {
			func less_than(other: Card): boolean = this.rank < other.rank
		}
		func first(xs: #[Card]): Card = xs[0]

		func beats(a: Card, b: Card): boolean = {
			let xs = #[a, b]
			let lt = xs[0] < xs[1]
			let head = first(xs)
			if (head.rank > 0) { lt } else { false }
		}
		"#,
	);
}

#[test]
fn golden_return_early_multiple_branches() {
	// `return` (4E) in more than one guard branch of the same function body.
	compile_ok(
		r#"
		func classify(n: int): int = {
			if (n < 0) { return -1 }
			if (n == 0) { return 0 }
			1
		}
		"#,
	);
}

#[test]
fn golden_shadowing_multi_step_chain() {
	// A same-scope re-let chain three deep (4E): each `let y` renames past the
	// previous JS binding rather than colliding.
	compile_ok(
		r#"
		func transform(x: int): int = {
			let y = x + 1
			let y = y * 2
			let y = y - 3
			y
		}
		"#,
	);
}

#[test]
fn golden_module_let_graph_with_mutual_recursion() {
	// A top-level `let` (4E) whose initializer calls into a pair of mutually
	// recursive module functions.
	compile_ok(
		r#"
		func is_even(n: int): boolean = if (n == 0) { true } else { is_odd(n - 1) }
		func is_odd(n: int): boolean = if (n == 0) { false } else { is_even(n - 1) }
		let ten_is_even = is_even(10)
		func demo(): boolean = ten_is_even
		"#,
	);
}

#[test]
fn golden_method_own_generic_bound_satisfying() {
	// An instance method's OWN generic bound (4G-b), called with a concrete
	// argument that satisfies it.
	compile_ok(
		r#"
		interface Area { func area(): int }
		struct Square(side: int)
		impl Area for Square { func area(): int = this.side * this.side }
		struct Box(v: int) { func apply<U: Area>(u: U): int = u.area() }
		func total(b: Box, s: Square): int = b.apply(s)
		"#,
	);
}

#[test]
fn golden_method_own_generic_bound_forwarding() {
	// The classic generic-to-generic forwarding case (4G-b): the caller's own
	// `T: Area` bound satisfies the callee method's identical requirement.
	compile_ok(
		r#"
		interface Area { func area(): int }
		struct Box(v: int) { func apply<U: Area>(u: U): int = u.area() }
		func outer<T: Area>(b: Box, x: T): int = b.apply(x)
		"#,
	);
}

#[test]
fn golden_struct_ctor_bound_satisfying() {
	// A struct constructor's declared generic bound (4G-b), constructed with a
	// concrete type argument that satisfies it.
	compile_ok(
		r#"
		interface Area { func area(): int }
		struct Square(side: int)
		impl Area for Square { func area(): int = this.side * this.side }
		struct Container<T: Area>(value: T)
		func make(s: Square): Container<Square> = Container(value = s)
		func demo(s: Square): int = make(s).value.area()
		"#,
	);
}

#[test]
fn golden_enum_ctor_bound_satisfying() {
	// An enum (field-variant) constructor's declared generic bound (4G-b),
	// constructed with a satisfying concrete type argument.
	compile_ok(
		r#"
		interface Area { func area(): int }
		struct Square(side: int)
		impl Area for Square { func area(): int = this.side * this.side }
		enum Holder<T: Area> { Some(value: T), Empty }
		func make(s: Square): Holder<Square> = Holder.Some(value = s)
		"#,
	);
}

#[test]
fn golden_strings_escapes_and_interpolation() {
	// String expressions (4H): escapes alongside a string interpoland and an
	// int interpoland in one literal.
	compile_ok(
		r#"
		func greet(name: string, n: int): string = "Hello, ${name}! n=${n}\n"
		"#,
	);
}

#[test]
fn golden_string_pattern_match_with_escapes() {
	// String PATTERNS (4H) may carry escapes too, reusing the same cooking as
	// string expressions.
	compile_ok(
		r#"
		func classify(s: string): int = match (s) {
			"a\nb" -> 1,
			"tab\there" -> 2,
			_ -> 0,
		}
		"#,
	);
}

#[test]
fn golden_for_loop_ranges_all_shapes() {
	// `for` loops over numeric ranges (4H): exclusive, inclusive, and a
	// parenthesized binary-expression bound.
	compile_ok(
		r#"
		func sum_exclusive(n: int): int = {
			let mut total = 0
			for (i in 1..n) { total = total + i }
			total
		}
		func sum_inclusive(n: int): int = {
			let mut total = 0
			for (i in 1..=n) { total = total + i }
			total
		}
		func sum_paren_binary(a: int, b: int, n: int): int = {
			let mut total = 0
			for (i in (a + b)..n) { total = total + i }
			total
		}
		"#,
	);
}

#[test]
fn golden_strings_equality_concat_compound_append() {
	// String equality (native `===`), `+` concatenation, and `+=` compound
	// append (4H) in one program.
	compile_ok(
		r#"
		func label(a: string, b: string): string = {
			let mut s = a
			s += "-"
			s += b
			s
		}
		func same(a: string, b: string): boolean = a == b
		"#,
	);
}

#[test]
fn golden_combo_enum_default_method_for_loop_string_builder() {
	// Combination: an enum implementing an interface default method, called
	// inside a `for` loop that builds a string via compound `+=` append.
	compile_ok(
		r#"
		interface Describable {
			func label(): string
			func tagged(): string = "[${this.label()}]"
		}
		enum Item { Widget(id: int), Gadget(id: int) }
		impl Describable for Item {
			func label(): string = match (this) {
				Widget(id) -> "w${id}",
				Gadget(id) -> "g${id}",
			}
		}
		func build_report(n: int): string = {
			let mut report = ""
			for (i in 0..n) {
				report += Item.Widget(id = i).tagged()
			}
			report
		}
		"#,
	);
}

// ═══════════════════════════════════════════════════════════════════════════
// Tier 2: run-tier programs (executed under Node, stdout asserted)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn golden_run_vec2_operator_suite() {
	// Binary +, -, scaling by int, unary negate, and compound += on one struct.
	let src = r#"
		interface MyPlus<Other, Output> { func plus(other: Other): Output }
		interface MyMinus<Other, Output> { func minus(other: Other): Output }
		interface MyTimes<Other, Output> { func times(other: Other): Output }
		interface MyNegate<Output> { func negate(): Output }

		struct Vec2(x: int, y: int) {
			impl MyPlus<Other = Vec2, Output = Vec2> {
				func plus(other: Vec2): Vec2 = Vec2(x = this.x + other.x, y = this.y + other.y)
			}
			impl MyMinus<Other = Vec2, Output = Vec2> {
				func minus(other: Vec2): Vec2 = Vec2(x = this.x - other.x, y = this.y - other.y)
			}
		}
		impl MyTimes<Other = int, Output = Vec2> for Vec2 {
			func times(scale: int): Vec2 = Vec2(x = this.x * scale, y = this.y * scale)
		}
		impl MyNegate<Output = Vec2> for Vec2 {
			func negate(): Vec2 = Vec2(x = -this.x, y = -this.y)
		}

		func combine(a: Vec2, b: Vec2): Vec2 = {
			let mut acc = (a + b) * 2
			acc += -(b - a)
			acc
		}
	"#;
	// a=(1,2), b=(3,4): (a+b)*2 = (8,12); -(b-a) = (-2,-2); acc = (6,10).
	assert_eq!(
		run(
			src,
			"combine(new Vec2({ x: 1, y: 2 }), new Vec2({ x: 3, y: 4 })).x"
		),
		"6"
	);
	assert_eq!(
		run(
			src,
			"combine(new Vec2({ x: 1, y: 2 }), new Vec2({ x: 3, y: 4 })).y"
		),
		"10"
	);
}

#[test]
fn golden_run_shape_area_match() {
	// Enum + match with bindings and a guard, driven through a struct method.
	let src = r#"
		enum Shape { Circle(r: int), Rectangle(w: int, h: int), Dot }

		func area(s: Shape): int = match (s) {
			Circle(r) -> 3 * r * r,
			Rectangle(w, h) if (w == h) -> w * w,
			Rectangle(w, h) -> w * h,
			Dot -> 0,
		}
	"#;
	assert_eq!(run(src, "area(Shape.Circle({ r: 2 }))"), "12");
	assert_eq!(run(src, "area(Shape.Rectangle({ w: 5, h: 5 }))"), "25");
	assert_eq!(run(src, "area(Shape.Rectangle({ w: 2, h: 3 }))"), "6");
	assert_eq!(run(src, "area(Shape.Dot)"), "0");
}

#[test]
fn golden_run_state_machine() {
	// Enum state machine stepped in a while loop; nullary variants are frozen
	// singletons so `===` against the variant is the identity check.
	let src = r#"
		enum Light { Red, Yellow, Green }

		func next(l: Light): Light = match (l) {
			Red -> Green,
			Green -> Yellow,
			Yellow -> Red,
		}

		func step_n(start: Light, n: int): Light = {
			let mut state = start
			let mut i = 0
			while (i < n) {
				state = next(state)
				i += 1
			}
			state
		}
	"#;
	assert_eq!(run(src, "step_n(Light.Red, 3) === Light.Red"), "true");
	assert_eq!(run(src, "step_n(Light.Red, 1) === Light.Green"), "true");
	assert_eq!(run(src, "step_n(Light.Red, 2) === Light.Yellow"), "true");
}

#[test]
fn golden_run_generic_bound_dispatch() {
	// A bounded generic function dispatching a (non-operator) interface method
	// on two different concrete impls.
	let src = r#"
		interface Area { func area(): int }

		struct Square(side: int)
		impl Area for Square { func area(): int = this.side * this.side }

		struct Rect(w: int, h: int)
		impl Area for Rect { func area(): int = this.w * this.h }

		func bigger<T: Area, U: Area>(a: T, b: U): int = {
			let first = a.area()
			let second = b.area()
			if (first > second) { first } else { second }
		}
	"#;
	assert_eq!(
		run(
			src,
			"bigger(new Square({ side: 4 }), new Rect({ w: 3, h: 5 }))"
		),
		"16"
	);
	assert_eq!(
		run(
			src,
			"bigger(new Square({ side: 2 }), new Rect({ w: 3, h: 5 }))"
		),
		"15"
	);
}

#[test]
fn golden_run_interface_default_method() {
	// A materialized interface default (`less_than` defined only in the
	// interface) drives both the `<` operator and an explicit call.
	let src = r#"
		interface MyComparable<Other> {
			func compare_to(other: Other): int
			func less_than(other: Other): boolean = this.compare_to(other) < 0
		}

		struct Version(major: int, minor: int)
		impl MyComparable<Other = Version> for Version {
			func compare_to(other: Version): int =
				if (this.major != other.major) { this.major - other.major }
				else { this.minor - other.minor }
		}

		func outdated(a: Version, b: Version): boolean = a < b
		func explicit(a: Version, b: Version): boolean = a.less_than(b)
	"#;
	let v1 = "new Version({ major: 1, minor: 9 })";
	let v2 = "new Version({ major: 2, minor: 0 })";
	assert_eq!(run(src, &format!("outdated({v1}, {v2})")), "true");
	assert_eq!(run(src, &format!("outdated({v2}, {v1})")), "false");
	assert_eq!(run(src, &format!("explicit({v1}, {v2})")), "true");
}

#[test]
fn golden_run_collections_pipeline() {
	// Lists, nested indexing, tuples, and int-keyed maps flowing through one
	// computation.
	let src = r#"
		func score(): int = {
			let grid = #[#[1, 2], #[3, 4]]
			let weights = #{ 0: 10, 1: 20 }
			let point = #(grid[1][0], grid[0][1])
			point[0] * weights[0] + point[1] * weights[1]
		}
	"#;
	// grid[1][0]=3, grid[0][1]=2 → 3*10 + 2*20 = 70.
	assert_eq!(run(src, "score()"), "70");
}

#[test]
fn golden_run_list_patterns() {
	// List patterns end-to-end: exact-empty, single, rest-with-suffix.
	let src = r#"
		func summarize(xs: #[int]): int = match (xs) {
			#[] -> -1,
			#[only] -> only * 100,
			#[first, ...mid, last] -> first + last,
			_ -> 0,
		}
	"#;
	assert_eq!(run(src, "summarize([])"), "-1");
	assert_eq!(run(src, "summarize([7])"), "700");
	assert_eq!(run(src, "summarize([10, 5, 5, 20])"), "30");
}

#[test]
fn golden_run_late_pinned_inference() {
	// The late-pinned empty list: the operator recorded against an unbound
	// inference variable re-resolves to the user impl once pinned, and actually
	// dispatches at runtime.
	let src = r#"
		interface MyComparable<Other> { func less_than(other: Other): boolean }
		struct Card(rank: int)
		impl MyComparable<Other = Card> for Card {
			func less_than(other: Card): boolean = this.rank < other.rank
		}

		func beats(a: Card, b: Card): boolean = {
			let xs = #[a, b]
			let lt = xs[1] < xs[0]
			let pin: #[Card] = xs
			lt
		}
	"#;
	assert_eq!(
		run(src, "beats(new Card({ rank: 10 }), new Card({ rank: 3 }))"),
		"true"
	);
	assert_eq!(
		run(src, "beats(new Card({ rank: 3 }), new Card({ rank: 10 }))"),
		"false"
	);
}

#[test]
fn golden_run_recursion() {
	// Direct and mutual recursion produce the classic answers.
	let src = r#"
		func fact(n: int): int = if (n <= 1) { 1 } else { n * fact(n - 1) }
		func fib(n: int): int = if (n < 2) { n } else { fib(n - 1) + fib(n - 2) }
		func is_even(n: int): boolean = if (n == 0) { true } else { is_odd(n - 1) }
		func is_odd(n: int): boolean = if (n == 0) { false } else { is_even(n - 1) }
	"#;
	assert_eq!(run(src, "fact(5)"), "120");
	assert_eq!(run(src, "fib(10)"), "55");
	assert_eq!(run(src, "is_even(8)"), "true");
	assert_eq!(run(src, "is_odd(8)"), "false");
}

#[test]
fn golden_run_match_ranges_and_unions() {
	// Range and union patterns in a grading function.
	let src = r#"
		func grade(score: int): int = match (score) {
			90..=100 -> 1,
			80..90 -> 2,
			60..80 -> 3,
			0 | 1 | 2 -> 5,
			_ -> 4,
		}
	"#;
	assert_eq!(run(src, "grade(95)"), "1");
	assert_eq!(run(src, "grade(90)"), "1");
	assert_eq!(run(src, "grade(85)"), "2");
	assert_eq!(run(src, "grade(65)"), "3");
	assert_eq!(run(src, "grade(1)"), "5");
	assert_eq!(run(src, "grade(42)"), "4");
}

#[test]
fn golden_run_bit_manipulation() {
	// Bitwise ops (incl. `~` and compound shift) verified against real values.
	let src = r#"
		func swap_nibbles(b: int): int = (b & 15) << 4 | (b >> 4) & 15
		func clear_flag(mask: int, flag: int): int = mask & ~flag
		func popcount(x: int): int = {
			let mut n = x
			let mut count = 0
			while (n != 0) {
				count += n & 1
				n >>= 1
			}
			count
		}
	"#;
	assert_eq!(run(src, "swap_nibbles(0xAB)"), "186"); // 0xAB -> 0xBA
	assert_eq!(run(src, "clear_flag(0b1111, 0b0100)"), "11");
	assert_eq!(run(src, "popcount(0b1011011)"), "5");
}

#[test]
fn golden_run_inventory_program() {
	// The combined inventory program from the compile tier, executed.
	let src = r#"
		enum Verdict { Full, Short(missing: int) }

		struct Item(sku: int, count: int) {
			func short_by(needed: int): int =
				if (this.count >= needed) { 0 } else { needed - this.count }
		}

		func check(items: #[Item], needed: int): Verdict = {
			let mut missing = 0
			let mut i = 0
			while (i < 2) {
				missing += items[i].short_by(needed)
				i += 1
			}
			if (missing == 0) { Full } else { Short(missing = missing) }
		}

		func penalty(v: Verdict): int = match (v) {
			Full -> 0,
			Short(missing) if (missing > 10) -> missing * 2,
			Short(missing) -> missing,
		}

		func demo(needed: int): int = {
			let stock = #[Item(sku = 1, count = 3), Item(sku = 2, count = 9)]
			penalty(check(stock, needed))
		}
	"#;
	assert_eq!(run(src, "demo(3)"), "0"); // both items suffice
	assert_eq!(run(src, "demo(5)"), "2"); // item 1 short by 2
	assert_eq!(run(src, "demo(15)"), "36"); // short 12+6=18 > 10 → doubled
}

#[test]
fn golden_run_enum_inherent_method_match_this() {
	// Enum inherent method reading its payload via `match (this)` (4D).
	let src = r#"
		enum Shape { Circle(r: int), Square(s: int) }
		impl Shape {
			func area(): int = match (this) {
				Circle(r) -> 3 * r * r,
				Square(s) -> s * s,
			}
		}
		func total(a: Shape, b: Shape): int = a.area() + b.area()
	"#;
	assert_eq!(
		run(src, "total(Shape.Circle({ r: 2 }), Shape.Square({ s: 3 }))"),
		"21"
	);
}

#[test]
fn golden_run_enum_operator_impl() {
	// An operator impl on an enum (4D + 4B) actually dispatches at runtime.
	let src = r#"
		interface MyPlus<Other, Output> { func plus(other: Other): Output }
		enum Count { Zero, Val(n: int) }
		impl MyPlus<Other = Count, Output = Count> for Count {
			func plus(other: Count): Count = match (this) {
				Zero -> other,
				Val(n = a) -> match (other) {
					Zero -> Val(n = a),
					Val(n = b) -> Val(n = a + b),
				},
			}
		}
		func combine(a: Count, b: Count): Count = a + b
	"#;
	assert_eq!(
		run(src, "combine(Count.Val({ n: 3 }), Count.Val({ n: 4 })).n"),
		"7"
	);
	assert_eq!(run(src, "combine(Count.Zero, Count.Val({ n: 5 })).n"), "5");
}

#[test]
fn golden_run_enum_interface_default_method() {
	// An interface default materialized on an enum impl (4C-b + 4D) runs.
	let src = r#"
		interface Describable {
			func label(): int
			func doubled_label(): int = this.label() * 2
		}
		enum Level { Low, High }
		impl Describable for Level {
			func label(): int = match (this) {
				Low -> 1,
				High -> 2,
			}
		}
		func demo(l: Level): int = l.doubled_label()
	"#;
	assert_eq!(run(src, "demo(Level.Low)"), "2");
	assert_eq!(run(src, "demo(Level.High)"), "4");
}

#[test]
fn golden_run_bare_variant_pattern_disambiguated_by_scrutinee_type() {
	// `Tree` and `Plant` both declare a `Leaf`/bare-shared-name variant, so `Leaf`/
	// `Branch` alone are globally ambiguous — but the `t: Tree`-typed scrutinee
	// pins the enum (type-directed variant resolution), so the bare arms need no
	// `Tree.` prefix, and the emitted JS actually runs and dispatches correctly.
	let src = r#"
		enum Tree { Leaf, Branch }
		enum Plant { Leaf, Root }

		func describe(t: Tree): int = match (t) {
			Leaf -> 0,
			Branch -> 1,
		}
	"#;
	assert_eq!(run(src, "describe(Tree.Leaf)"), "0");
	assert_eq!(run(src, "describe(Tree.Branch)"), "1");
}

#[test]
fn golden_run_bare_variant_construction_disambiguated_by_expected_type() {
	// A bare nullary variant construction (`Leaf`, no `Tree.` prefix) disambiguates
	// against the enum pinned by the enclosing return-type annotation, even though
	// another enum shares the variant name.
	let src = r#"
		enum Tree { Leaf, Branch }
		enum Plant { Leaf, Root }

		func make(): Tree = Leaf

		func tag(t: Tree): int = match (t) {
			Leaf -> 0,
			Branch -> 1,
		}
	"#;
	assert_eq!(run(src, "tag(make())"), "0");
}

#[test]
fn golden_run_interface_default_override_wins_at_runtime() {
	// Override-wins (4C-b), verified with an override that computes something
	// DIFFERENT from what the default would: default's `compare_to`-based
	// `less_than` would say `10 < 3` is false, but the override unconditionally
	// returns `true` — the runtime result can only be `true` if the override,
	// not the default, is what actually got called.
	let src = r#"
		interface MyComparable<Other> {
			func compare_to(other: Other): int
			func less_than(other: Other): boolean = this.compare_to(other) < 0
		}
		struct Weird(v: int)
		impl MyComparable<Other = Weird> for Weird {
			func compare_to(other: Weird): int = this.v - other.v
			func less_than(other: Weird): boolean = true
		}
		func check(a: Weird, b: Weird): boolean = a < b
	"#;
	assert_eq!(
		run(src, "check(new Weird({ v: 10 }), new Weird({ v: 3 }))"),
		"true"
	);
}

#[test]
fn golden_run_module_let_graph_mutual_recursion() {
	// A top-level `let` whose initializer calls into mutually recursive
	// module functions (4E) — the whole init graph must actually execute.
	let src = r#"
		func is_even(n: int): boolean = if (n == 0) { true } else { is_odd(n - 1) }
		func is_odd(n: int): boolean = if (n == 0) { false } else { is_even(n - 1) }
		let ten_is_even = is_even(10)
		let eleven_is_even = is_even(11)
		func demo(): boolean = ten_is_even
		func demo2(): boolean = eleven_is_even
	"#;
	assert_eq!(run(src, "demo()"), "true");
	assert_eq!(run(src, "demo2()"), "false");
}

#[test]
fn golden_run_method_own_generic_bound() {
	// Method own-generic bounds (4G-b), both satisfying and forwarding shapes,
	// actually execute and dispatch to the right impl.
	let src = r#"
		interface Area { func area(): int }
		struct Square(side: int)
		impl Area for Square { func area(): int = this.side * this.side }
		struct Box(v: int) { func apply<U: Area>(u: U): int = u.area() }
		func total(b: Box, s: Square): int = b.apply(s)
		func outer<T: Area>(b: Box, x: T): int = b.apply(x)
	"#;
	assert_eq!(
		run(src, "total(new Box({ v: 1 }), new Square({ side: 4 }))"),
		"16"
	);
	assert_eq!(
		run(src, "outer(new Box({ v: 1 }), new Square({ side: 3 }))"),
		"9"
	);
}

#[test]
fn golden_run_ctor_bounds_struct_and_enum() {
	// Struct and enum constructor generic bounds (4G-b), both satisfying,
	// exercised end-to-end.
	let src = r#"
		interface Area { func area(): int }
		struct Square(side: int)
		impl Area for Square { func area(): int = this.side * this.side }
		struct Container<T: Area>(value: T)
		enum Holder<T: Area> { Some(value: T), Empty }
		func demo(s: Square): int = Container(value = s).value.area()
		func demo_enum(s: Square): int = match (Holder.Some(value = s)) {
			Holder.Some(value) -> value.area(),
			Empty -> 0,
		}
	"#;
	assert_eq!(run(src, "demo(new Square({ side: 5 }))"), "25");
	assert_eq!(run(src, "demo_enum(new Square({ side: 6 }))"), "36");
}

#[test]
fn golden_run_strings_escapes_interpolation_and_patterns() {
	// Escapes, string + int interpolation, string equality/concat/compound
	// append, and a string PATTERN with escapes, all executed under Node.
	let src = r#"
		func greet(name: string, n: int): string = "Hello, ${name}! n=${n}\n"
		func label(a: string, b: string): string = {
			let mut s = a
			s += "-"
			s += b
			s
		}
		func same(a: string, b: string): boolean = a == b
		func classify(s: string): int = match (s) {
			"a\nb" -> 1,
			"tab\there" -> 2,
			_ -> 0,
		}
	"#;
	assert_eq!(run(src, r#"greet("World", 5)"#), "Hello, World! n=5");
	assert_eq!(run(src, r#"label("x", "y")"#), "x-y");
	assert_eq!(run(src, r#"same("x", "x")"#), "true");
	assert_eq!(run(src, r#"same("x", "y")"#), "false");
	assert_eq!(run(src, r#"classify("a\nb")"#), "1");
	assert_eq!(run(src, r#"classify("tab\there")"#), "2");
	assert_eq!(run(src, r#"classify("nope")"#), "0");
}

#[test]
fn golden_run_for_loop_ranges_all_shapes() {
	// `for` loops (4H) over exclusive, inclusive, and parenthesized-bound
	// ranges, verified against the classic 1..=n triangular-number sums.
	let src = r#"
		func sum_exclusive(n: int): int = {
			let mut total = 0
			for (i in 1..n) { total = total + i }
			total
		}
		func sum_inclusive(n: int): int = {
			let mut total = 0
			for (i in 1..=n) { total = total + i }
			total
		}
		func sum_paren_binary(a: int, b: int, n: int): int = {
			let mut total = 0
			for (i in (a + b)..n) { total = total + i }
			total
		}
	"#;
	assert_eq!(run(src, "sum_exclusive(5)"), "10"); // 1+2+3+4
	assert_eq!(run(src, "sum_inclusive(5)"), "15"); // 1+2+3+4+5
	assert_eq!(run(src, "sum_paren_binary(1, 1, 5)"), "9"); // 2+3+4
}

#[test]
fn golden_run_combo_enum_default_method_for_loop_string_builder() {
	// Combination end-to-end: an enum's interface default method, called
	// inside a `for` loop that builds a string with compound `+=` append.
	let src = r#"
		interface Describable {
			func label(): string
			func tagged(): string = "[${this.label()}]"
		}
		enum Item { Widget(id: int), Gadget(id: int) }
		impl Describable for Item {
			func label(): string = match (this) {
				Widget(id) -> "w${id}",
				Gadget(id) -> "g${id}",
			}
		}
		func build_report(n: int): string = {
			let mut report = ""
			for (i in 0..n) {
				report += Item.Widget(id = i).tagged()
			}
			report
		}
	"#;
	assert_eq!(run(src, "build_report(3)"), "[w0][w1][w2]");
}

// ═══════════════════════════════════════════════════════════════════════════
// Findings: programs that SHOULD work but don't (kept, ignored, reported)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn golden_finding_return_statement_ices_in_lowering() {
	// (`0 - n` rather than a line-leading `-n`, which would continue the previous
	// expression as a binary minus — the parse gotcha, not the finding.)
	compile_ok(
		r#"
		func abs(n: int): int = {
			if (n >= 0) { return n }
			0 - n
		}
		"#,
	);
}

#[test]
fn golden_finding_let_shadowing_emits_invalid_js() {
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
fn golden_finding_top_level_let_is_silently_dropped() {
	let src = r#"
		let answer = 42
		func f(): int = answer
	"#;
	assert_eq!(run(src, "f()"), "42");
}

#[test]
fn golden_finding_impl_trait_param_rejects_concrete_argument() {
	// FINDING (fixed by Slice 4F): calling a function whose param uses `impl
	// Trait` sugar (`shape: Area`) with a concrete impl'ing type used to be
	// rejected — `mismatched types: expected `T268435456`, found `Square`` (the
	// synthetic bound param never instantiated at call sites); declaring/using
	// it inside the body always worked fine.
	let src = r#"
		interface Area { func area(): int }
		struct Square(side: int)
		impl Area for Square {
			func area(): int = this.side * this.side
		}
		func measure(shape: Area): int = shape.area()
		func total(s: Square): int = measure(s)
		"#;
	assert_eq!(run(src, "total(new Square({ side: 3 }))"), "9");
}
