//! Golden regression corpus: realistic known-good Nymph programs pinned so
//! future slices cannot silently regress them.
//!
//! Two tiers:
//! - **Compile-clean tier**: each program must compile with zero diagnostics and
//!   without panicking (a lowering panic here means a slice regressed a feature
//!   that used to work).
//! - **Run tier**: the emitted JS also executes under `node`, asserting stdout.
//!
//! The corpus deliberately stays inside the implemented surface (Slices 0–4C-c).
//! Known deferrals it must NOT touch: string literals in expression position,
//! closures, range *expressions* (range patterns are fine), `as` casts, `?`/`!`
//! postfix, `??`/`in`/`!in`/`|>`, user `==`/`!=` dispatch, namespaced/static
//! methods, mut methods, positional variant construction, enum methods/impls,
//! blanket-impl materialization, bounded-generic *operator* dispatch, stdlib
//! imports.
//!
//! Parse gotchas honored throughout: `if` requires parens; match arms use `->`
//! and commas; a guard whose expression ends in an identifier must be
//! parenthesized (otherwise `ident -> body` parses as a closure); line-leading
//! operators continue the previous expression.

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
		interface Plus<Other, Output> { func plus(other: Other): Output }
		interface Minus<Other, Output> { func minus(other: Other): Output }
		interface Times<Other, Output> { func times(other: Other): Output }

		struct Vec2(x: int, y: int) {
			impl Plus<Other = Vec2, Output = Vec2> {
				func plus(other: Vec2): Vec2 = Vec2(x = this.x + other.x, y = this.y + other.y)
			}
		}

		impl Minus<Other = Vec2, Output = Vec2> for Vec2 {
			func minus(other: Vec2): Vec2 = Vec2(x = this.x - other.x, y = this.y - other.y)
		}

		impl Times<Other = int, Output = Vec2> for Vec2 {
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
		interface Negate<Output> { func negate(): Output }
		interface Not<Output> { func not(): Output }
		interface BitNot<Output> { func bit_not(): Output }

		struct Vec2(x: int, y: int)
		impl Negate<Output = Vec2> for Vec2 {
			func negate(): Vec2 = Vec2(x = -this.x, y = -this.y)
		}

		struct Tristate(known: boolean, value: boolean)
		impl Not<Output = Tristate> for Tristate {
			func not(): Tristate = Tristate(known = this.known, value = !this.value)
		}

		struct Mask(bits: int)
		impl BitNot<Output = Mask> for Mask {
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
		interface Plus<Other, Output> { func plus(other: Other): Output }

		struct Money(cents: int)
		impl Plus<Other = Money, Output = Money> for Money {
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
		interface Comparable<Other> {
			func compare_to(other: Other): int
			func less_than(other: Other): boolean = this.compare_to(other) < 0
		}

		struct Version(major: int, minor: int)
		impl Comparable<Other = Version> for Version {
			func compare_to(other: Version): int =
				if (this.major != other.major) { this.major - other.major }
				else { this.minor - other.minor }
		}

		struct Priority(level: int)
		impl Comparable<Other = Priority> for Priority {
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
	// construction, and variant names shared across enums.
	compile_ok(
		r#"
		enum Status { Active, Suspended(reason_code: int), Closed }
		enum Option<T> { Some(value: T), None }
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
	// `->` as a closure), and wildcard fallthrough.
	compile_ok(
		r#"
		enum Option<T> { Some(value: T), None }
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
		interface Comparable<Other> { func less_than(other: Other): boolean }
		struct Card(rank: int)
		impl Comparable<Other = Card> for Card {
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
		interface Plus<Other, Output> { func plus(other: Other): Output }
		interface Area { func area(): int }

		struct Vec2(x: int, y: int) {
			impl Plus<Other = Vec2, Output = Vec2> {
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
	compile_ok(
		r#"
		enum Verdict { Ok, Short(missing: int) }

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
			if (missing == 0) { Ok } else { Short(missing = missing) }
		}

		func penalty(v: Verdict): int = match (v) {
			Ok -> 0,
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

// ═══════════════════════════════════════════════════════════════════════════
// Tier 2: run-tier programs (executed under Node, stdout asserted)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn golden_run_vec2_operator_suite() {
	// Binary +, -, scaling by int, unary negate, and compound += on one struct.
	let src = r#"
		interface Plus<Other, Output> { func plus(other: Other): Output }
		interface Minus<Other, Output> { func minus(other: Other): Output }
		interface Times<Other, Output> { func times(other: Other): Output }
		interface Negate<Output> { func negate(): Output }

		struct Vec2(x: int, y: int) {
			impl Plus<Other = Vec2, Output = Vec2> {
				func plus(other: Vec2): Vec2 = Vec2(x = this.x + other.x, y = this.y + other.y)
			}
			impl Minus<Other = Vec2, Output = Vec2> {
				func minus(other: Vec2): Vec2 = Vec2(x = this.x - other.x, y = this.y - other.y)
			}
		}
		impl Times<Other = int, Output = Vec2> for Vec2 {
			func times(scale: int): Vec2 = Vec2(x = this.x * scale, y = this.y * scale)
		}
		impl Negate<Output = Vec2> for Vec2 {
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
		interface Comparable<Other> {
			func compare_to(other: Other): int
			func less_than(other: Other): boolean = this.compare_to(other) < 0
		}

		struct Version(major: int, minor: int)
		impl Comparable<Other = Version> for Version {
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
		interface Comparable<Other> { func less_than(other: Other): boolean }
		struct Card(rank: int)
		impl Comparable<Other = Card> for Card {
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
		enum Verdict { Ok, Short(missing: int) }

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
			if (missing == 0) { Ok } else { Short(missing = missing) }
		}

		func penalty(v: Verdict): int = match (v) {
			Ok -> 0,
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
