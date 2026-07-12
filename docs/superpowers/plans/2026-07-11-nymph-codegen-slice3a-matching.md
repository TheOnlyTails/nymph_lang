# Codegen Slice 3A (Pattern Matching — scalar & variant core) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Compile `match` over scalars and enum variants to a JS if/else-if chain so pattern matching runs correctly under Node, leveraging the Slice 2C Symbol-tag ABI for variant tests.

**Architecture:** A typed `HirPat` tree (Wildcard / Binding / Lit / Variant) is produced by lowering and compiled by codegen into, per arm, a **test** expression and a **binding sequence** against a scrutinee temporary. `match` lowers to `HirExpr::Match`; codegen emits `const _s = <scrutinee>; let _r; if (<test0>) { <binds0>; _r = <body0> } else if … else { <bindsN>; _r = <bodyN> }` → `_r`. Exhaustiveness is guaranteed by sema, so the last arm needs no test. Variant patterns are ambiguous by bare name, and patterns carry no `NodeId`, so the checker records each variant pattern's resolved `(enum, variant)` in a **span-keyed** side-table that lowering consumes.

**Tech Stack:** Rust (nightly), oxc 0.139 (`AstBuilder` + `Codegen`), jj VCS, Node for execution tests.

## Global Constraints

- **Toolchain:** every cargo command uses `cargo +nightly` (shell pins stable 1.96.0, which fails to build).
- **VCS is jj**, not git. Commit with `jj commit -m "line1" -m "line2" …` (never `$(cat <<EOF)` — `cat` is aliased to `bat` and corrupts messages). Path-scope with trailing path args. Read commits with `jj --no-pager`.
- **oxc is 0.139**; `AstBuilder` construction API is `#[deprecated]` (module-scoped allow in `emit.rs`); verify every builder call by compiling.
- **Codegen stays type-free.** Lowering bakes decisions into HIR node shapes; `emit.rs` never consults types/annotations.
- Commit trailer: `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`.

## Design Decisions (scope)

- **Patterns supported in 3A:** `_` (placeholder), binding (`x`, `x = sub`), scalar literals (`int`/`uint`/`float`/`bool`/`char`), and variant patterns — nullary (`None`, `Opt.None`) and field-carrying (`Some(value)`, `Opt.Some(value = p)`) with nested field sub-patterns.
- **Deferred to Slice 3B (lowering panics loudly, consistent with existing deferrals):** guards (`if cond`), tuple/list/map/struct patterns, range patterns, string patterns, union patterns (`A | B`), and the standalone `is` expression.
- **Variant-pattern resolution is checker-recorded, span-keyed.** Patterns have no `NodeId`; every written pattern has a unique `Span`. The checker records `(enum, variant)` at the pattern's span (for both nullary-binding and struct-path variant forms); lowering reads by span. Bare and qualified variant patterns both resolve soundly this way.
- **Variant test uses the 2C ABI:** `_s?.[TAG] === <Enum>.<Variant>[TAG]` — pure symbol identity, no string reconstruction. `TAG` is the module const; a variant pattern implies the module has an enum, so `TAG` is in scope.
- **`match` value ABI:** result-temporary hoisting (`let _r; if(…){…_r=…} …`) — the same shape Slice 1 used for `if`/`while` in value position.

---

## Task 1: HIR match/arm/pattern nodes

**Files:**
- Modify: `crates/nymph-hir/src/hir.rs`
- Modify: `crates/nymph-codegen/src/emit.rs` (temporary `unreachable!` arm)
- Modify: `crates/nymph-sema/src/lower_hir.rs` (temporary — the match arm stays a panic until Task 3; no HirModule field change here)

**Interfaces:**
- Produces: `HirExpr::Match { scrutinee: Box<HirExpr>, arms: Vec<HirArm> }`; `HirArm { pat: HirPat, body: HirExpr }`; `HirPat::{Wildcard, Binding { name: EcoString, sub: Option<Box<HirPat>> }, Lit(HirLit), Variant { enum_name: EcoString, variant: EcoString, fields: Vec<(EcoString, HirPat)> }}`; `HirLit::{Num(f64), Bool(bool), Char(char)}`.

- [ ] **Step 1: Add the nodes**

In `crates/nymph-hir/src/hir.rs`, add to `enum HirExpr`:

```rust
	/// `match <scrutinee> { <arms> }` — compiled to an if/else-if chain.
	Match {
		scrutinee: Box<HirExpr>,
		arms: Vec<HirArm>,
	},
```

Add the supporting types:

```rust
#[derive(Clone, Debug, PartialEq)]
pub struct HirArm {
	pub pat: HirPat,
	pub body: HirExpr,
}

/// A compiled pattern. Codegen turns each into a test expression plus a binding
/// sequence against a subject expression.
#[derive(Clone, Debug, PartialEq)]
pub enum HirPat {
	/// `_` — always matches, binds nothing.
	Wildcard,
	/// Bind the subject to `name`, then match `sub` against it (if present).
	Binding {
		name: EcoString,
		sub: Option<Box<HirPat>>,
	},
	/// A scalar literal — matches by `===`.
	Lit(HirLit),
	/// A variant — matches by tag identity, then matches each field sub-pattern
	/// against the corresponding field of the subject.
	Variant {
		enum_name: EcoString,
		variant: EcoString,
		fields: Vec<(EcoString, HirPat)>,
	},
}

#[derive(Clone, Debug, PartialEq)]
pub enum HirLit {
	Num(f64),
	Bool(bool),
	Char(char),
}
```

- [ ] **Step 2: Keep `emit.rs` compiling**

In `emit.rs`, `emit_expr` currently routes control-flow (`Block`/`If`/`While`) to `emit_value(...).into_expression(...)`. Add `HirExpr::Match { .. }` to that same value-position arm (it will be handled by `emit_value` in Task 4). To keep Task 1 self-contained and compiling before Task 4, add a temporary explicit arm in `emit_expr`:

```rust
			HirExpr::Match { .. } => unreachable!("emitted in slice-3a Task 4"),
```

and in `emit_value`, leave `Match` to fall through to its `other => …` default for now (Task 4 replaces this). If the compiler reports a non-exhaustive match in `emit_value`, add a temporary `HirExpr::Match { .. } => unreachable!("emitted in slice-3a Task 4")` there too.

- [ ] **Step 3: Build**

Run: `cargo +nightly build -p nymph-codegen`
Expected: compiles (the lowering `match` arm is still the pre-existing `other => panic!` catch-all, so no lowering change is needed yet).

- [ ] **Step 4: Commit**

```bash
cargo +nightly fmt -p nymph-hir -p nymph-codegen
jj commit -m "feat(hir): match/arm/pattern nodes (HirExpr::Match, HirArm, HirPat, HirLit)" -m "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>" crates/nymph-hir crates/nymph-codegen
```

---

## Task 2: Checker records variant-pattern resolution (span-keyed)

**Files:**
- Modify: `crates/nymph-sema/src/annotate.rs`
- Modify: `crates/nymph-sema/src/infer_pattern.rs`
- Test: `crates/nymph-sema/tests/annotate.rs`

**Interfaces:**
- Produces: `Annotations::pattern_variant_of(span: Span) -> Option<&VariantResolution>`; recorded for every variant pattern (nullary-binding and struct-path forms).
- Consumes: existing `VariantResolution`, `resolve_pattern_path`, `nullary_variant_pattern`, `DefData.name`, `VariantSig.name`.

- [ ] **Step 1: Add the span-keyed side-table**

In `annotate.rs`, add a third map to `Annotations` (keeps `ExprInfo` `Copy`; import `nymph_ast::Span`):

```rust
	pattern_variants: FxHashMap<Span, VariantResolution>,
```

Add methods:

```rust
	/// Record which `(enum, variant)` a variant *pattern* resolved to, keyed by the
	/// pattern's source span (patterns carry no NodeId, but each written pattern has
	/// a unique span).
	pub(crate) fn record_pattern_variant(&mut self, span: Span, res: VariantResolution) {
		self.pattern_variants.insert(span, res);
	}

	pub fn pattern_variant_of(&self, span: Span) -> Option<&VariantResolution> {
		self.pattern_variants.get(&span)
	}
```

- [ ] **Step 2: Write the failing test**

Add to `crates/nymph-sema/tests/annotate.rs`:

```rust
#[test]
fn records_variant_pattern_resolution() {
	let module = parse(
		r#"
		enum Opt { Some(value: int), None }
		func f(o: Opt): int = match (o) {
			Some(value) => value
			None => 0
		}
		"#,
	);
	let checked = check_module(&module);
	assert!(checked.diags.is_empty(), "{:?}", checked.diags);

	// Reach the match arms' patterns and assert each variant pattern is recorded.
	let nymph_ast::decl::Declaration::Func { body, .. } = &module.members[1] else {
		panic!("expected func");
	};
	let nymph_ast::expr::ExprKind::Match { arms, .. } = &body.kind else {
		panic!("expected match, got {:?}", body.kind);
	};
	let mut found = 0;
	for arm in arms {
		if let Some(res) = checked.annotations.pattern_variant_of(arm.pattern.1) {
			assert_eq!(res.enum_name, "Opt");
			assert!(res.variant == "Some" || res.variant == "None");
			found += 1;
		}
	}
	assert_eq!(found, 2, "both Some(value) and None patterns recorded");
}
```

> Verify the `match` surface syntax against `crates/nymph-sema/tests/` (condition parenthesization, arm separators — commas vs newlines). Adjust the source to what parses. `arm.pattern.1` is the pattern's `Span` (`Spanned<Pattern>` = `(Pattern, Span)`).

- [ ] **Step 3: Run; expect failure**

Run: `cargo +nightly test -p nymph-sema --test annotate records_variant_pattern_resolution`
Expected: FAIL — `pattern_variant_of` returns `None`.

- [ ] **Step 4: Record at the two pattern-variant sites**

In `infer_pattern.rs`, add a helper mirroring the expr-side one (reuse `Checker::variant_resolution` from `infer_expr.rs` — it is already `&self` and returns `VariantResolution`):

- In `nullary_variant_pattern`, on the `Ok((enum_def, variant))` nullary branch (where it returns `Some(adt)`), record: `let res = self.variant_resolution(enum_def, variant); self.annotations.record_pattern_variant(span, res);` before returning. `nullary_variant_pattern` already receives `span`.
- In `pattern_struct`, on the `Some(PatternTarget::Variant(enum_def, variant))` branch, record at `span` the same way (before the `for field` loop).

> `variant_resolution` lives in `infer_expr.rs` as `fn variant_resolution(&self, DefId, usize) -> VariantResolution`. It is a method on `Checker`, so it is callable from `infer_pattern.rs`. If its visibility is private-to-module, widen it to `pub(crate)` within the `impl` (same crate, so `pub(crate)` suffices).

- [ ] **Step 5: Run test + full sema suite**

Run: `cargo +nightly test -p nymph-sema`
Expected: PASS — the new recording test plus all existing.

- [ ] **Step 6: fmt, clippy, commit**

```bash
cargo +nightly fmt -p nymph-sema && cargo +nightly clippy -p nymph-sema --all-targets
jj commit -m "feat(sema): record variant-pattern resolution in a span-keyed side-table" -m "Patterns carry no NodeId, so variant-pattern (enum, variant) resolution is recorded by the pattern's unique span for lowering to consume. Covers nullary-binding and struct-path variant patterns." -m "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>" crates/nymph-sema
```

---

## Task 3: Lower `match` and patterns

**Files:**
- Modify: `crates/nymph-sema/src/lower_hir.rs`
- Test: `crates/nymph-sema/tests/lower_hir.rs`

**Interfaces:**
- Consumes: `HirExpr::Match`, `HirArm`, `HirPat`, `HirLit` (Task 1); `Annotations::pattern_variant_of` (Task 2); `ExprKind::Match { value, arms }`, `MatchArm { pattern, guard, body }`, `Pattern`, `StructPatternField`.

- [ ] **Step 1: Write the failing lowering test**

Add to `crates/nymph-sema/tests/lower_hir.rs`:

```rust
#[test]
fn lowers_match_over_enum() {
	use nymph_hir::hir::{HirArm, HirPat};
	let hir = lower(
		r#"
		enum Opt { Some(value: int), None }
		func unwrap_or(o: Opt): int = match (o) {
			Some(value) => value
			None => 0
		}
		"#,
	);
	let f = hir.funcs.iter().find(|f| f.name == "unwrap_or").unwrap();
	let HirExpr::Match { arms, .. } = &f.body else {
		panic!("expected Match, got {:?}", f.body);
	};
	assert_eq!(arms.len(), 2);
	// First arm: Some(value) → a Variant pattern binding `value`.
	let HirArm { pat: HirPat::Variant { enum_name, variant, fields }, .. } = &arms[0] else {
		panic!("expected Variant pattern, got {:?}", arms[0].pat);
	};
	assert_eq!(enum_name, "Opt");
	assert_eq!(variant, "Some");
	assert_eq!(fields.len(), 1);
	assert_eq!(fields[0].0, "value");
	assert!(matches!(&fields[0].1, HirPat::Binding { name, sub: None } if name == "value"));
	// Second arm: None → a nullary Variant pattern.
	assert!(matches!(&arms[1].pat, HirPat::Variant { variant, fields, .. } if variant == "None" && fields.is_empty()));
}
```

- [ ] **Step 2: Run; expect failure**

Run: `cargo +nightly test -p nymph-sema --test lower_hir lowers_match_over_enum`
Expected: FAIL — `match` currently hits the `other => panic!("slice-2a lowering does not yet handle …")` catch-all.

- [ ] **Step 3: Lower `match` and add `lower_pattern`**

In `lower_hir.rs`, add the `ExprKind::Match` arm to `lower_expr` (place it before the `other =>` catch-all):

```rust
			ExprKind::Match { value, arms } => {
				let arms = arms
					.iter()
					.map(|arm| {
						assert!(
							arm.guard.is_none(),
							"slice-3a match lowering does not yet handle guards"
						);
						HirArm {
							pat: self.lower_pattern(&arm.pattern),
							body: self.lower_expr(&arm.body),
						}
					})
					.collect();
				HirExpr::Match {
					scrutinee: Box::new(self.lower_expr(value)),
					arms,
				}
			}
```

Add `lower_pattern` (place near `lower_expr`):

```rust
	fn lower_pattern(&self, pat: &nymph_ast::Spanned<nymph_ast::expr::Pattern>) -> HirPat {
		use nymph_ast::expr::{Pattern, StructPatternField};
		match &pat.0 {
			Pattern::Placeholder => HirPat::Wildcard,
			Pattern::Int(v) => HirPat::Lit(HirLit::Num(v.0 as f64)),
			Pattern::UInt(v) => HirPat::Lit(HirLit::Num(v.0 as f64)),
			Pattern::Float(v) => HirPat::Lit(HirLit::Num(v.0.into_inner())),
			Pattern::Boolean(b) => HirPat::Lit(HirLit::Bool(b.0)),
			Pattern::Char(c) => HirPat::Lit(HirLit::Char(c.0)),
			Pattern::Grouped(inner) => self.lower_pattern(inner),
			Pattern::Binding { name, inner } => {
				// A bare name recorded as a variant is a nullary variant pattern; else a
				// binding (optionally with a sub-pattern).
				if let Some(res) = self.annotations.pattern_variant_of(pat.1) {
					HirPat::Variant {
						enum_name: res.enum_name.clone(),
						variant: res.variant.clone(),
						fields: Vec::new(),
					}
				} else {
					let sub = match &inner.0 {
						Pattern::Placeholder => None,
						_ => Some(Box::new(self.lower_pattern(inner))),
					};
					HirPat::Binding {
						name: name.0.clone(),
						sub,
					}
				}
			}
			Pattern::Struct { fields, .. } => {
				let res = self
					.annotations
					.pattern_variant_of(pat.1)
					.expect("slice-3a struct-path patterns must resolve to a variant (struct patterns deferred)");
				let lowered = fields
					.iter()
					.filter_map(|f| match &f.0 {
						StructPatternField::Value { name, value } => {
							Some((name.0.clone(), self.lower_pattern(value)))
						}
						StructPatternField::Named(name) => Some((
							name.0.clone(),
							HirPat::Binding {
								name: name.0.clone(),
								sub: None,
							},
						)),
						StructPatternField::Rest => None,
					})
					.collect();
				HirPat::Variant {
					enum_name: res.enum_name.clone(),
					variant: res.variant.clone(),
					fields: lowered,
				}
			}
			other => panic!("slice-3a lowering does not yet handle pattern {other:?}"),
		}
	}
```

Add `HirArm, HirLit, HirPat` to the `nymph_hir::hir` import.

- [ ] **Step 4: Run lowering test + sema suite**

Run: `cargo +nightly test -p nymph-sema`
Expected: PASS.

- [ ] **Step 5: fmt, clippy, commit**

```bash
cargo +nightly fmt -p nymph-sema && cargo +nightly clippy -p nymph-sema --all-targets
jj commit -m "feat(sema): lower match to HirExpr::Match and patterns to HirPat" -m "Scalar/binding/placeholder/variant patterns (nested); struct-path patterns resolve to variants via the span-keyed side-table. Guards and aggregate/range/string/union patterns panic (deferred to 3B)." -m "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>" crates/nymph-sema
```

---

## Task 4: Compile patterns + emit `match`, run under Node

**Files:**
- Modify: `crates/nymph-codegen/src/emit.rs`
- Test: `crates/nymph-codegen/tests/run_node.rs`

**Interfaces:**
- Consumes: `HirExpr::Match`, `HirArm`, `HirPat`, `HirLit`.

- [ ] **Step 1: Write the failing Node-execution tests**

Add to `crates/nymph-codegen/tests/run_node.rs`:

```rust
#[test]
fn runs_match_variant_binding() {
	// `match` over an enum, binding a field variant's payload.
	let src = r#"
		enum Opt { Some(value: int), None }
		func unwrap_or(o: Opt): int = match (o) {
			Some(value) => value
			None => 0
		}
	"#;
	assert_eq!(run(src, "unwrap_or(Opt.Some({ value: 42 }))"), "42");
	assert_eq!(run(src, "unwrap_or(Opt.None)"), "0");
}

#[test]
fn runs_match_literal_and_wildcard() {
	let src = r#"
		func classify(n: int): int = match (n) {
			0 => 100
			1 => 200
			_ => 300
		}
	"#;
	assert_eq!(run(src, "classify(0)"), "100");
	assert_eq!(run(src, "classify(1)"), "200");
	assert_eq!(run(src, "classify(9)"), "300");
}
```

> The JS driver constructs a variant via the emitted factory: `Opt.Some({ value: 42 })` (object arg) and `Opt.None` (singleton). Confirm against the 2C ABI shape.

- [ ] **Step 2: Run; expect failure**

Run: `cargo +nightly test -p nymph-codegen --test run_node runs_match_literal_and_wildcard`
Expected: FAIL — `match` hits the Task 1 `unreachable!`.

- [ ] **Step 3: Compile patterns to (test, bindings)**

In `emit.rs`, add a pattern compiler producing a JS boolean test (`None` ⇒ always true) and a list of `(name, value-expression)` bindings against a subject expression. Because oxc expression nodes are arena values that cannot be cloned cheaply, pass the **subject as a HIR-independent closure/params** — simplest is to recompute the subject expression from a small `Subject` enum the compiler can re-emit (e.g. `Subject::Temp(&str)` for the scrutinee temp, `Subject::Field(Box<Subject>, EcoString)` for `_s.field`). Provide `fn emit_subject(&self, s: &Subject) -> Expression<'a>` that builds a fresh expression each time it is needed (tests and bindings each need their own copy).

```rust
enum Subject {
	Temp(String),
	Field(Box<Subject>, String),
}
```

`emit_subject`:
- `Temp(name)` → `self.ast.expression_identifier(SPAN, alloc_str(name))`
- `Field(base, f)` → `member_expression_static(emit_subject(base), identifier_name(f), false)`

Pattern compiler (returns `(Option<Expression>, Vec<(String /*name*/, Subject)>)` — bindings as `(name, subject)` so the const decl is emitted later via `emit_subject`):

```rust
fn compile_pat(&self, pat: &HirPat, subj: &Subject) -> (Option<Expression<'a>>, Vec<(String, Subject)>) {
	match pat {
		HirPat::Wildcard => (None, vec![]),
		HirPat::Binding { name, sub } => {
			let mut binds = vec![(name.to_string(), subj.clone())];
			let test = match sub {
				None => None,
				Some(sub) => {
					let (t, mut b) = self.compile_pat(sub, subj);
					binds.append(&mut b);
					t
				}
			};
			(test, binds)
		}
		HirPat::Lit(lit) => {
			let subject = self.emit_subject(subj);
			let value = self.emit_lit(lit);
			// subject === value
			(Some(self.ast.expression_binary(SPAN, subject, BinaryOperator::StrictEquality, value)), vec![])
		}
		HirPat::Variant { enum_name, variant, fields } => {
			// subject?.[TAG] === <enum>.<variant>[TAG]
			let tag_of_subject = self.tag_read(self.emit_subject(subj)); // subject?.[TAG]
			let tag_of_variant = self.tag_read_plain(self.variant_member(enum_name, variant)); // <E>.<V>[TAG]
			let mut test = self.ast.expression_binary(SPAN, tag_of_subject, BinaryOperator::StrictEquality, tag_of_variant);
			let mut binds = vec![];
			for (field, sub) in fields {
				let field_subj = Subject::Field(Box::new(subj.clone()), field.to_string());
				let (t, mut b) = self.compile_pat(sub, &field_subj);
				binds.append(&mut b);
				if let Some(t) = t {
					// test = test && t
					test = self.ast.expression_logical(SPAN, test, LogicalOperator::And, t);
				}
			}
			(Some(test), binds)
		}
	}
}
```

Add `Subject: Clone` (derive), `emit_lit` (Num/Bool/Char → numeric/boolean/string literal, mirroring `emit_expr`'s scalar arms), and two tag-read helpers:
- `tag_read(obj)` → `obj?.[TAG]` — an **optional** computed member (`member_expression_static`/computed member with `optional: true`, key = identifier `TAG`). Verify the oxc computed-member builder and its `optional` flag by compiling; model on the existing `Index` emitter (`alloc_computed_member_expression`).
- `tag_read_plain(obj)` → `obj[TAG]` — a non-optional computed member (variant bindings are always defined).

> Both tag reads index by the `TAG` identifier (the module const). Use `expression_identifier(SPAN, "TAG")` as the computed property expression.

- [ ] **Step 4: Emit `match` as a value (if/else-if chain)**

Add a `HirExpr::Match { scrutinee, arms }` case to `emit_value` (producing a `JsValue`), and route `emit_expr`'s `Match` arm to `emit_value(expr).into_expression(self.ast)` (join it with the existing `Block | If | While` value-position arm; remove the temporary Task 1 `unreachable!`).

`emit_value` for `Match`:

```rust
HirExpr::Match { scrutinee, arms } => {
	let s = self.gensym();                 // scrutinee temp
	let r = self.gensym();                 // result temp
	let mut stmts = self.ast.vec();
	stmts.push(self.const_decl(&s, self.emit_expr(scrutinee)));   // const _s = <scrutinee>;
	stmts.push(self.let_uninit(self.ast.allocator.alloc_str(&r)));// let _r;
	let subj = Subject::Temp(s);
	// Build the if/else-if chain from the LAST arm backwards; the last arm (totality)
	// needs no test → it is the final block.
	let mut chain: Option<Statement<'a>> = None;
	for (i, arm) in arms.iter().enumerate().rev() {
		let (test, binds) = self.compile_pat(&arm.pat, &subj);
		let block = self.arm_block(&r, &binds, &arm.body);   // { <binds>; _r = <body>; }
		let is_last = i + 1 == arms.len();
		chain = Some(if is_last && chain.is_none() {
			block                          // final arm: unconditional
		} else {
			let cond = test.unwrap_or_else(|| self.ast.expression_boolean_literal(SPAN, true));
			self.ast.statement_if(SPAN, cond, block, chain.take())
		});
	}
	if let Some(chain) = chain {
		stmts.push(chain);
	}
	JsValue { stmts, expr: self.ast.expression_identifier(SPAN, self.ast.allocator.alloc_str(&r)) }
}
```

Add `arm_block(result_name, binds, body)` → a JS block `{ const <n> = <subject>; …; _r = <body-expr>; }` (the body via `emit_value` so a block/if body works; assign its `expr` to `_r`, prefixed by its `stmts` and the binding const-decls). Model on the existing `assign_block` helper (which already builds `{ …; _r = …; }`).

> `let_uninit`, `const_decl`, `assign_block`, and `gensym` already exist in `emit.rs`. The chain-from-the-back construction keeps each `else` branch as the previously-built statement. Verify `statement_if(span, cond, then, Option<else>)` against oxc 0.139.

- [ ] **Step 5: Run execution tests**

Run: `cargo +nightly test -p nymph-codegen --test run_node`
Expected: PASS — `unwrap_or(Some 42)`→42, `unwrap_or(None)`→0, `classify` 100/200/300, plus all prior tests.

- [ ] **Step 6: Full workspace gate + fmt + commit**

Run: `cargo +nightly test && cargo +nightly clippy --all-targets`
Expected: green/clean (ignore pre-existing warnings in crates you don't own).

```bash
cargo +nightly fmt -p nymph-codegen
jj commit -m "feat(codegen): compile patterns to test+bindings and emit match as an if-chain; run under Node" -m "match lowers to const _s=<scrutinee>; let _r; if(<test>){<binds>; _r=<body>} else …; variant tests use the 2C ABI (_s?.[TAG] === E.V[TAG]); totality lets the last arm skip its test. Node: unwrap_or(Some 42)->42, None->0; literal+wildcard classify." -m "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>" crates/nymph-codegen
```

---

## Self-Review

**Spec coverage (design §"Kept structured (first-class HIR nodes)" — match subset):**
- `HirExpr::Match { scrutinee, arms }` with typed `HirPat` ✓
- Each arm compiled to test + binding sequence ✓ (`compile_pat`)
- Variant tests via the Symbol-tag identity `x?.[TAG] === E.V[TAG]` ✓
- Totality assumed (last arm testless) ✓
- Value-position match via result-temp hoisting ✓

**Deferred to Slice 3B, correctly:** guards; tuple/list/map/struct patterns; range/string/union patterns; the standalone `is` expression. Each panics loudly in lowering (consistent with the codebase's existing not-yet-implemented deferrals), never silently miscompiles.

**Placeholder scan:** no "TBD"/"handle edge cases". oxc 0.139 builders for optional computed member (`_s?.[TAG]`), `statement_if`, and logical/binary expressions are given at reference shapes with a compile-to-verify note. The `Subject` re-emission approach is explicit (arena expressions can't be cheaply cloned, so bindings carry a re-emittable `Subject` rather than a built `Expression`).

**Type consistency:** `HirExpr::Match { scrutinee: Box<HirExpr>, arms: Vec<HirArm> }`; `HirArm { pat: HirPat, body: HirExpr }`; `HirPat::{Wildcard, Binding{name, sub}, Lit(HirLit), Variant{enum_name, variant, fields}}`; `HirLit::{Num, Bool, Char}`; `Annotations::pattern_variant_of(Span) -> Option<&VariantResolution>`. Consistent across Tasks 1–4.

**Scope:** one coherent, Node-testable increment — scalar and variant matching, the core payoff of the Slice 2C ABI. Aggregate patterns, guards, and `is` are a separate 3B follow-on.
