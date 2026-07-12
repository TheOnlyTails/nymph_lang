# Codegen Slice 3B (Pattern Matching — full) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `match` fully general — add struct, tuple, list (with rest), map, range, string, and union patterns, plus guards — so every checker-accepted pattern compiles and runs under Node.

**Architecture:** Extend `HirPat` with the remaining pattern shapes and add a guard to `HirArm`. Guards require a matched-but-guard-failed arm to fall through to the next arm, which an `if/else-if` chain cannot express — so the match emission is rewritten to a **labeled block with `break`**: `_m: { if (test0) { <binds0>; if (guard0) { _r = body0; break _m; } } … { <bindsLast>; _r = bodyLast; } }`. Element access for tuple/list patterns adds `Subject::Index`. No checker changes: guards already type-check, structural patterns need no resolution, and a `Pattern::Struct` with no recorded variant is a struct pattern.

**Tech Stack:** Rust (nightly), oxc 0.139 (`AstBuilder` + `Codegen`), jj VCS, Node for execution tests.

## Global Constraints

- **Toolchain:** every cargo command uses `cargo +nightly` (shell pins stable 1.96.0, which fails to build).
- **VCS is jj**, not git. Commit with `jj commit -m "line1" -m "line2" …` (never `$(cat <<EOF)` — `cat` is aliased to `bat` and corrupts messages). Path-scope with trailing path args. Read commits with `jj --no-pager`.
- **oxc is 0.139**; `AstBuilder` construction API is `#[deprecated]` (module-scoped allow in `emit.rs`); verify every builder call by compiling.
- **Codegen stays type-free.** Lowering bakes decisions into HIR node shapes; `emit.rs` never consults types/annotations.
- **Match-arm syntax is `pattern -> body,`** (arrow + comma), guards are `pattern if <cond> -> body`. Enum variant fields construct via object-arg factories (`Enum.V({ f: … })`); nullary variants are `Enum.V`.
- Commit trailer: `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`.

## Design Decisions (scope)

- **Added pattern forms:** struct (`Point(x = px, y = py)`), tuple (`#(a, b)`), list (`#[]`, `#[a, b]`, `#[a, ...rest]`, `#[a, ...rest, b]`), map (`#{ 1: v }` — **literal keys only**), range (`1..10`, `1..=10`, `1..`, `..10`, `..=10` over int/char literal bounds), string (**text-only** — no interpolation/complex escapes), and union (`A | B`).
- **Guards** (`pattern if cond -> body`): compiled via the labeled-break rewrite.
- **Struct & tuple patterns are irrefutable** (the nominal/arity type guarantees the shape): no test, only bindings. List/map/range/string patterns are refutable (length/has/bounds/equality tests). Variant patterns keep the 2C tag test.
- **Union (`A | B`)** — 3B supports unions whose sub-patterns **bind nothing** (literals, nullary variants, ranges, strings): test is `testA || testB`. A union whose sub-pattern binds panics in lowering (deferred — cross-branch binding needs consistent-name analysis).
- **Map keys must be literals** (`HirLit`); a non-literal map-pattern key panics in lowering.
- **String patterns text-only**; an interpolated or escape-bearing string pattern panics in lowering.
- **No checker changes.** Lowering distinguishes struct-vs-variant `Pattern::Struct` via `pattern_variant_of` (variant ⇒ Some, struct ⇒ None).

---

## Task 1: HIR pattern variants, guard, and Subject::Index

**Files:**
- Modify: `crates/nymph-hir/src/hir.rs`
- Modify: `crates/nymph-codegen/src/emit.rs` (temporary `unreachable!` arms + `Subject::Index`)

**Interfaces:**
- Produces: `HirArm { pat, guard: Option<HirExpr>, body }`; `HirLit::Str(EcoString)`; `HirPat::{Struct { fields }, Tuple(Vec<HirPat>), List { prefix, rest: Option<Option<EcoString>>, suffix }, Map(Vec<(HirLit, HirPat)>), Range(HirRange), Or(Box<HirPat>, Box<HirPat>)}`; `HirRange`.

- [ ] **Step 1: Extend the HIR**

In `crates/nymph-hir/src/hir.rs`:

```rust
// Add a guard to HirArm:
pub struct HirArm {
	pub pat: HirPat,
	/// A `pattern if <cond>` guard — the arm matches only when this is truthy.
	pub guard: Option<HirExpr>,
	pub body: HirExpr,
}

// Add to HirLit:
	Str(EcoString),

// Add to HirPat:
	/// A struct pattern — irrefutable (the nominal type guarantees the shape); binds
	/// each named field.
	Struct { fields: Vec<(EcoString, HirPat)> },
	/// A tuple pattern — irrefutable, binds each element by index.
	Tuple(Vec<HirPat>),
	/// A list pattern `#[<prefix>, ...rest, <suffix>]`. `rest` present ⇒ a spread
	/// (with an optional binding); absent ⇒ exact length.
	List {
		prefix: Vec<HirPat>,
		rest: Option<Option<EcoString>>,
		suffix: Vec<HirPat>,
	},
	/// A map pattern — tests `.has(key)` and matches the value pattern against `.get(key)`.
	Map(Vec<(HirLit, HirPat)>),
	/// A range pattern over scalar bounds.
	Range(HirRange),
	/// `A | B` — matches if either side matches (3B: neither side binds).
	Or(Box<HirPat>, Box<HirPat>),

// New:
#[derive(Clone, Debug, PartialEq)]
pub enum HirRange {
	From(HirLit),
	To(HirLit),
	ToInclusive(HirLit),
	Exclusive { min: HirLit, max: HirLit },
	Inclusive { min: HirLit, max: HirLit },
}
```

- [ ] **Step 2: Keep `emit.rs` compiling**

Add `Subject::Index(Box<Subject>, usize)` to the `Subject` enum and handle it in `emit_subject` (`<base>[<i>]` — a computed member with a numeric-literal property; model on the `Index`/`tag_read` computed-member emitters). Add temporary arms so the crate compiles until Tasks 3–4:
- `compile_pat`: `HirPat::Struct { .. } | HirPat::Tuple(_) | HirPat::List { .. } | HirPat::Map(_) | HirPat::Range(_) | HirPat::Or(..) => unreachable!("compiled in slice-3b Task 3/4")`.
- `emit_lit`: `HirLit::Str(s) => self.ast.expression_string_literal(SPAN, self.ast.allocator.alloc_str(s), None)`.
- The existing `HirArm` destructuring in `emit_value`'s Match uses `arm.pat`/`arm.body`; it now must also read `arm.guard` — leave guard unused for now (Task 3 wires it), but confirm the field access compiles.

- [ ] **Step 3: Build**

Run: `cargo +nightly build -p nymph-codegen`
Expected: compiles (lowering still produces only the 3A pattern shapes, so the new `unreachable!` arms are never hit yet — but a `HirArm` literal now needs a `guard` field; the only constructor is `lower_hir`, updated in Task 2, so temporarily add `guard: None` at the one `HirArm { … }` site in `lower_hir.rs` to build).

- [ ] **Step 4: Commit**

```bash
cargo +nightly fmt -p nymph-hir -p nymph-codegen -p nymph-sema
jj commit -m "feat(hir): struct/tuple/list/map/range/union pattern nodes + arm guards" -m "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>" crates/nymph-hir crates/nymph-codegen crates/nymph-sema
```

---

## Task 2: Lower guards and the new pattern forms

**Files:**
- Modify: `crates/nymph-sema/src/lower_hir.rs`
- Test: `crates/nymph-sema/tests/lower_hir.rs`

**Interfaces:**
- Consumes: the Task 1 HIR; `Pattern::{Struct, Tuple, List, Map, Range, String, Union}`, `ListPatternEntry`, `MapPatternEntry`, `RangePatternKind`, `StringPatternPart`.

- [ ] **Step 1: Write failing lowering tests**

Add to `crates/nymph-sema/tests/lower_hir.rs` (one test per family; assert on the lowered `HirPat` shape). Example for tuple + struct + guard:

```rust
#[test]
fn lowers_tuple_struct_and_guard_patterns() {
	use nymph_hir::hir::{HirArm, HirPat};
	let hir = lower(
		r#"
		struct Point(x: int, y: int)
		func f(p: #(int, int)): int = match (p) {
			#(0, y) -> y,
			#(x, _) if x > 0 -> x,
			#(x, _) -> 0,
		}
		func g(pt: Point): int = match (pt) {
			Point(x = px, y = _) -> px,
		}
	"#,
	);
	let f = hir.funcs.iter().find(|f| f.name == "f").unwrap();
	let HirExpr::Match { arms, .. } = &f.body else { panic!() };
	assert!(matches!(&arms[0].pat, HirPat::Tuple(elems) if elems.len() == 2));
	assert!(arms[1].guard.is_some(), "second arm carries a guard");
	let g = hir.funcs.iter().find(|f| f.name == "g").unwrap();
	let HirExpr::Match { arms, .. } = &g.body else { panic!() };
	assert!(matches!(&arms[0].pat, HirPat::Struct { fields } if fields.len() == 2));
}
```

Add similar tests for list (`#[a, ...rest]` → `HirPat::List { prefix, rest: Some(_), .. }`), range (`1..10` → `HirPat::Range(HirRange::Exclusive { .. })`), string (`"hi" -> …` → `HirPat::Lit(HirLit::Str(_))`), and union (`Red | Green` over a nullary-variant enum → `HirPat::Or(..)`). Verify each source parses against `crates/nymph-syntax/tests/`.

- [ ] **Step 2: Run; expect failure**

Run: `cargo +nightly test -p nymph-sema --test lower_hir lowers_tuple_struct_and_guard_patterns`
Expected: FAIL — the new pattern forms hit `lower_pattern`'s `other => panic!` and guards hit the `assert!(arm.guard.is_none())`.

- [ ] **Step 3: Lower guards**

In the `ExprKind::Match` arm of `lower_expr`, replace the `assert!(arm.guard.is_none(), …)` with `guard: arm.guard.as_ref().map(|g| self.lower_expr(g))`, and set it on the `HirArm`.

- [ ] **Step 4: Extend `lower_pattern`**

Add arms (a `lower_lit_pattern` helper converts a literal `Pattern` → `HirLit`, panicking on non-literals — reused by Map keys and Range bounds):

```rust
			Pattern::String(parts) => HirPat::Lit(HirLit::Str(lower_string_pattern(parts))),
			Pattern::Tuple(entries) => HirPat::Tuple(self.lower_pattern_items(entries)),
			Pattern::List(entries) => self.lower_list_pattern(entries),
			Pattern::Map(entries) => HirPat::Map(self.lower_map_pattern(entries)),
			Pattern::Range(kind) => HirPat::Range(self.lower_range_pattern(kind)),
			Pattern::Union(a, b) => HirPat::Or(
				Box::new(self.lower_pattern(a)),
				Box::new(self.lower_pattern(b)),
			),
```

- `Pattern::Struct` arm: when `pattern_variant_of(pat.1)` is `None`, produce `HirPat::Struct { fields }` (same field extraction as the variant case) instead of panicking.
- `lower_pattern_items(entries: &[Spanned<ListPatternEntry>]) -> Vec<HirPat>`: map `Item(p)` → `lower_pattern(p)`; panic on `Rest` (tuples have no rest).
- `lower_list_pattern`: split entries at the (at most one) `Rest`; `prefix` = items before, `rest` = `Some(name_opt)`, `suffix` = items after. Panic if more than one `Rest`.
- `lower_map_pattern`: each `Entry(k, v)` → `(lower_lit_pattern(k), lower_pattern(v))`; panic on `Rest` (map rest deferred) and non-literal keys.
- `lower_range_pattern(kind: &RangePatternKind) -> HirRange`: map each bound via `lower_lit_pattern`.
- `lower_string_pattern(parts) -> EcoString`: concatenate `StringPatternPart::Text`; panic on `EscapeSequence` (text-only in 3B).
- A `Union` whose lowered sub-pattern binds (contains a `Binding`/`Struct`/`Variant` with fields/…): to keep 3B tractable, `lower_pattern` may still build `HirPat::Or`, and the **codegen** rejects binding unions (Task 4) — OR detect here and panic. Prefer detecting in codegen so lowering stays a pure structural map.

- [ ] **Step 5: Run lowering tests + sema suite**

Run: `cargo +nightly test -p nymph-sema`
Expected: PASS.

- [ ] **Step 6: fmt, clippy, commit**

```bash
cargo +nightly fmt -p nymph-sema && cargo +nightly clippy -p nymph-sema --all-targets
jj commit -m "feat(sema): lower guards and struct/tuple/list/map/range/string/union patterns" -m "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>" crates/nymph-sema
```

---

## Task 3: Rewrite match emission for guards; compile struct/tuple patterns

**Files:**
- Modify: `crates/nymph-codegen/src/emit.rs`
- Test: `crates/nymph-codegen/tests/run_node.rs`

**Interfaces:**
- Consumes: `HirArm.guard`, `HirPat::{Struct, Tuple}`, `Subject::Index`.

- [ ] **Step 1: Write failing Node tests**

```rust
#[test]
fn runs_match_tuple_and_guard() {
	let src = r#"
		func f(p: #(int, int)): int = match (p) {
			#(0, y) -> y,
			#(x, _) if x > 10 -> x,
			#(x, _) -> 0,
		}
	"#;
	assert_eq!(run(src, "f([0, 7])"), "7");     // first arm
	assert_eq!(run(src, "f([20, 1])"), "20");   // guard passes
	assert_eq!(run(src, "f([5, 1])"), "0");     // guard fails → fall through
}

#[test]
fn runs_match_struct_pattern() {
	let src = r#"
		struct Point(x: int, y: int)
		func f(pt: Point): int = match (pt) {
			Point(x = px, y = py) -> px + py,
		}
	"#;
	assert_eq!(run(src, "f(new Point({ x: 3, y: 4 }))"), "7");
}
```

- [ ] **Step 2: Run; expect failure**

Run: `cargo +nightly test -p nymph-codegen --test run_node runs_match_tuple_and_guard`
Expected: FAIL — tuple/struct patterns hit the Task 1 `unreachable!`, and guards are still ignored.

- [ ] **Step 3: Rewrite `emit_value`'s `Match` to a labeled block**

Replace the if/else-if chain with a labeled block so guards can fall through:

```rust
HirExpr::Match { scrutinee, arms } => {
	let s = self.ast.allocator.alloc_str(&self.gensym());
	let r = self.ast.allocator.alloc_str(&self.gensym());
	let label = self.ast.allocator.alloc_str(&self.gensym());
	let mut stmts = self.ast.vec();
	let scrutinee_expr = self.emit_expr(scrutinee);
	stmts.push(self.const_decl(s, scrutinee_expr));
	stmts.push(self.let_uninit(r));
	let subj = Subject::Temp(s.to_string());

	let mut body = self.ast.vec();          // statements inside `<label>: { … }`
	for (i, arm) in arms.iter().enumerate() {
		let is_last = i + 1 == arms.len();
		let (test, binds) = self.compile_pat(&arm.pat, &subj);
		// inner: { <binds>; <r> = <body>; break <label>; }  (last arm omits break)
		let assign_and_break = self.arm_commit(r, &binds, &arm.body, label, is_last);
		match (&arm.guard, test, is_last) {
			// Last arm, no guard: unconditional tail (exhaustiveness guarantees it).
			(None, _, true) => body.push(assign_and_break),
			// Guarded or non-last: `if (<test?> && ) { <binds>; if (<guard?>) { commit } }`.
			_ => {
				let guard = arm.guard.as_ref().map(|g| self.emit_expr(g));
				body.push(self.arm_if(test, guard, assign_and_break));
			}
		}
	}
	let block = self.ast.statement_block(SPAN, body);
	let labeled = self
		.ast
		.statement_labeled(SPAN, self.ast.label_identifier(SPAN, label), block);
	stmts.push(labeled);
	JsValue { stmts, expr: self.ast.expression_identifier(SPAN, r) }
}
```

Add helpers:
- `arm_commit(r, binds, body, label, is_last)` → a **block** `{ const <binds>; <r> = <body>; break <label>; }` (omit `break` when `is_last`, since it's the tail and nothing follows). Bindings + body via the existing `arm_block` logic; append `statement_break(SPAN, Some(label_identifier(SPAN, label)))` unless `is_last`. Note bindings must be inside the guarded block so the guard can read them — see `arm_if`.
- `arm_if(test: Option<Expr>, guard: Option<Expr>, commit: Statement)` — builds the arm's conditional. Bindings needed for the guard live inside `commit`, but the guard must run *after* bindings, so structure as: if there is a guard, the commit block already contains `const <binds>`; wrap the `<r>=…; break;` part in `if (<guard>) { … }`. Simplest: fold the guard into `arm_commit` (pass `guard` in, emit `const <binds>; if (guard) { <r>=…; break; }`), and let `arm_if` handle only the outer pattern `test`. Reconcile the two helpers so bindings precede the guard. Concretely: `arm_commit(r, binds, body, guard, label, is_last)` emits `{ const <binds>; if (<guard or true>) { <r> = <body>; break <label>; } }`, and the outer `if (<test>) <that block>` is added only when `test` is `Some`.

> The key ordering constraint: **bindings before guard**. A guard like `#(x, _) if x > 10` reads `x`, which is bound from `_s[0]`; the binding const must be emitted before the guard `if`. Keeping both inside one block (`{ const x = _s[0]; if (x > 10) { … } }`) satisfies this. Verify `statement_labeled`, `statement_break`, `label_identifier` against oxc 0.139 (signatures confirmed: `statement_labeled(span, LabelIdentifier, Statement)`, `statement_break(span, Option<LabelIdentifier>)`).

- [ ] **Step 4: Compile struct & tuple patterns**

Add to `compile_pat`:

```rust
			HirPat::Struct { fields } => {
				let mut binds = Vec::new();
				for (field, sub) in fields {
					let field_subj = Subject::Field(Box::new(subj.clone()), field.to_string());
					let (t, mut b) = self.compile_pat(sub, &field_subj);
					debug_assert!(t.is_none(), "struct field sub-patterns are handled recursively");
					binds.append(&mut b);
					// A refutable field sub-pattern (e.g. a nested variant) contributes a test:
					if let Some(t) = t { /* AND into an accumulated test */ }
				}
				(/* accumulated test (None if all irrefutable) */, binds)
			}
			HirPat::Tuple(elems) => {
				let mut test: Option<Expression<'a>> = None;
				let mut binds = Vec::new();
				for (i, sub) in elems.iter().enumerate() {
					let elem_subj = Subject::Index(Box::new(subj.clone()), i);
					let (t, mut b) = self.compile_pat(sub, &elem_subj);
					binds.append(&mut b);
					if let Some(t) = t {
						test = Some(match test {
							None => t,
							Some(prev) => self.ast.expression_logical(SPAN, prev, LogicalOperator::And, t),
						});
					}
				}
				(test, binds)
			}
```

> Struct and tuple patterns are irrefutable *at their own level*, but a nested field/element sub-pattern can be refutable (e.g. `#(Some(x), _)`), so accumulate any sub-tests with `&&`. Factor the "AND an optional test into an accumulator" into a small helper used by Struct, Tuple, and Variant.

- [ ] **Step 5: Run tests (3A must still pass) + Step 6: fmt/clippy/commit**

Run: `cargo +nightly test -p nymph-codegen --test run_node`
Expected: PASS — new tuple/struct/guard tests plus all 3A match tests (the labeled-block rewrite must not regress them).

```bash
cargo +nightly fmt -p nymph-codegen && cargo +nightly clippy -p nymph-codegen --all-targets
jj commit -m "feat(codegen): labeled-block match emission with guards; struct/tuple patterns" -m "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>" crates/nymph-codegen
```

---

## Task 4: Compile list/map/range/string/union patterns

**Files:**
- Modify: `crates/nymph-codegen/src/emit.rs`
- Test: `crates/nymph-codegen/tests/run_node.rs`

**Interfaces:**
- Consumes: `HirPat::{List, Map, Range, Or}`, `HirLit::Str`, `HirRange`.

- [ ] **Step 1: Write failing Node tests**

```rust
#[test]
fn runs_match_list_patterns() {
	let src = r#"
		func f(xs: #[int]): int = match (xs) {
			#[] -> 0,
			#[x] -> x,
			#[a, ...rest] -> a,
			#[_, ..._] -> -1,
		}
	"#;
	assert_eq!(run(src, "f([])"), "0");
	assert_eq!(run(src, "f([7])"), "7");
	assert_eq!(run(src, "f([3, 4, 5])"), "3");
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
	let src = r#"
		enum Color { Red, Green, Blue }
		func warm(c: Color): boolean = match (c) {
			Red | Green -> true,
			Blue -> false,
		}
	"#;
	assert_eq!(run(src, "warm(Color.Red)"), "true");
	assert_eq!(run(src, "warm(Color.Blue)"), "false");
}
```

- [ ] **Step 2: Run; expect failure** — the new pattern kinds hit the Task 1 `unreachable!`.

- [ ] **Step 3: Compile the refutable patterns**

Add to `compile_pat` (verify every oxc builder — `.length`/`.has`/`.get`/`.slice` are `member_call`/`member_expression_static`; comparisons are `expression_binary` with `LessThan`/`LessEqualThan`/`GreaterEqualThan`; `||` is `expression_logical` with `LogicalOperator::Or`):

- **List `{ prefix, rest, suffix }`:**
  - length test: `rest.is_none()` ⇒ `_s.length === <prefix.len()>`; `rest.is_some()` ⇒ `_s.length >= <prefix.len() + suffix.len()>`.
  - prefix i ⇒ subject `Subject::Index(_s, i)`.
  - suffix j (from the end) ⇒ `_s[_s.length - <suffix.len()> + j]` — add `Subject::IndexFromEnd(base, offset_from_end)` (emit `_s[_s.length - k]`) or build the expression inline.
  - `rest = Some(Some(name))` ⇒ bind `name = _s.slice(<prefix.len()>, _s.length - <suffix.len()>)`.
  - AND all element sub-tests into the length test.
- **Map `Vec<(HirLit, HirPat)>`:** for each `(key, vpat)`: test `_s.has(<key>)`, and match `vpat` against `Subject`-for-`_s.get(<key>)` — add `Subject::MapGet(base, HirLit)` (emit `_s.get(<key>)`). AND the `has` tests with the value sub-tests.
- **Range(HirRange):** emit the bound comparisons against `_s` (`From(min)` ⇒ `min <= _s`; `To(max)` ⇒ `_s < max`; `ToInclusive(max)` ⇒ `_s <= max`; `Exclusive{min,max}` ⇒ `min <= _s && _s < max`; `Inclusive{min,max}` ⇒ `min <= _s && _s <= max`). Bounds via `emit_lit`.
- **Or(a, b):** compile both; **panic if either produces bindings** (3B supports only non-binding unions). Test = `testA || testB` (a `None` sub-test means that side is irrefutable ⇒ the whole `Or` is irrefutable ⇒ `None`).

Extend `Subject` and `emit_subject` with the `IndexFromEnd`, `MapGet` cases used above (each re-emits a fresh expression). Add a `bind` helper if the accumulator logic is shared.

- [ ] **Step 4: Run tests** — `cargo +nightly test -p nymph-codegen --test run_node` — PASS all.

- [ ] **Step 5: Full workspace gate + fmt + commit**

```bash
cargo +nightly test && cargo +nightly clippy --all-targets
cargo +nightly fmt -p nymph-codegen
jj commit -m "feat(codegen): compile list/map/range/string/union patterns; match is now fully general" -m "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>" crates/nymph-codegen
```

---

## Self-Review

**Spec coverage:** struct ✓, tuple ✓, list (exact + rest, prefix/suffix) ✓, map (literal keys) ✓, range (all five forms) ✓, string (text-only) ✓, union (non-binding) ✓, guards ✓. `match` now handles every checker-accepted pattern except the explicitly-deferred edges below.

**Deferred (panic loudly in lowering/codegen, never silent):** map-pattern `rest`; non-literal map keys; interpolated/escaped string patterns; union sub-patterns that bind. Each is a narrow edge; document in the commit.

**Placeholder scan:** no "TBD". The guard ordering constraint (bindings before guard) is stated explicitly; the labeled-break rewrite is justified (if/else-if can't express guard fall-through). oxc builders for labeled statements, `break`, list/map member calls, and comparisons are given at reference shapes with compile-to-verify notes; the confirmed signatures (`statement_labeled`, `statement_break`, `label_identifier`) are noted.

**Type consistency:** `HirArm { pat, guard: Option<HirExpr>, body }`; `HirPat::{…, Struct{fields}, Tuple(Vec), List{prefix,rest,suffix}, Map(Vec<(HirLit,HirPat)>), Range(HirRange), Or(Box,Box)}`; `HirLit::Str`; `HirRange`; `Subject::{Temp, Field, Index, IndexFromEnd, MapGet}`. Consistent across tasks.

**Scope:** completes `match`, building on 3A's compiler. The labeled-break rewrite is the one non-additive change and is covered by re-running all 3A tests in Task 3.
