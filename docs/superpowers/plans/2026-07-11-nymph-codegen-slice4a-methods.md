# Codegen Slice 4A (Inherent Instance Methods) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Compile inherent instance methods on structs (`impl Point { func norm(): float = … }` and struct-body-inner `func`s) to JS class methods, with `this` support, so method-bearing programs run under Node.

**Architecture:** Struct methods (from top-level `impl` blocks and struct-inner members) are collected during lowering and attached to the struct's `HirClass`; codegen emits them into the class body alongside the constructor. `this` lowers to a new `HirExpr::This` node emitting the JS `this` keyword. Method *calls* (`p.norm()`) already lower structurally through the existing `Call`→`Field` path (they emit `p.norm(args)`); no new call machinery is needed. **No checker changes:** inherent method bodies already type-check with `self_ty` set (see `crates/nymph-sema/src/members.rs` and the passing `members.rs` test `top_level_inherent_impl`), so their nodes carry annotations and lower correctly.

**Tech Stack:** Rust (nightly), oxc 0.139 (`AstBuilder` + `Codegen`), jj VCS, Node for execution tests.

## Global Constraints

- **Toolchain:** every cargo command uses `cargo +nightly` (shell pins stable 1.96.0, which fails to build).
- **VCS is jj**, not git. Commit with `jj commit -m "line1" -m "line2" …` (never `$(cat <<EOF)` — `cat` is aliased to `bat` and corrupts messages). Path-scope with trailing path args. Read commits with `jj --no-pager`.
- **oxc is 0.139**; `AstBuilder` construction API is `#[deprecated]` (module-scoped allow in `emit.rs`); verify every builder call by compiling. Class-method emission already exists in `emit_class` (the constructor uses `class_element_method_definition` — reuse that shape with `MethodDefinitionKind::Method`).
- **Codegen stays type-free.** Lowering bakes decisions into HIR node shapes; `emit.rs` never consults types/annotations.
- Commit trailer: `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`.

## Design Decisions (scope)

- **Inherent instance methods on structs only.** Sources: top-level `impl <Named> { func … }` (`Declaration::Impl` with `type_ = Type::Reference { name }`) and struct-body-inner methods (`StructInnerMember::Member(ImplMember::Func)`). Both become JS instance methods on the struct's class.
- **`this`** → `HirExpr::This` → the JS `this` keyword. `this.field` is `Field { recv: This, name }` → `this.field` (reuses 2B field access).
- **Method calls** (`p.norm(args)`) need no new lowering — the existing `Call { func: MemberAccess }` path already emits `p.norm(args)`. Method args are **positional** in 4A (labels dropped, as the generic call path does); labeled method args are deferred.
- **Deferred to later Slice 4 sub-slices:** operator overloading (`a + b` → `a.plus(b)` — needs the checker's Milestone-B operator resolution + `DispatchKind` recording); interface impls / `impl … for` / dispatch; **enum methods** (enums emit as the Symbol-tag object, not a class — methods on variant values need an ABI decision); namespaced/static methods (`Point.at(…)`); external companions (`.external.ts`); `mut` methods; range constructors; `??`/`?`/`as`/`is`.
- **No checker changes.** If a method body uses a not-yet-lowered construct, lowering panics loudly as elsewhere.

---

## Task 1: HIR method + `this` nodes

**Files:**
- Modify: `crates/nymph-hir/src/hir.rs`
- Modify: `crates/nymph-codegen/src/emit.rs` (temporary handling)
- Modify: `crates/nymph-sema/src/lower_hir.rs` (temporary `methods: Vec::new()` at the `HirClass` site)

**Interfaces:**
- Produces: `HirClass { name, fields, methods: Vec<HirMethod> }`; `HirMethod { name: EcoString, params: Vec<EcoString>, body: HirExpr }`; `HirExpr::This`.

- [ ] **Step 1: Extend the HIR**

In `crates/nymph-hir/src/hir.rs`, add `methods` to `HirClass` and a `HirMethod` struct:

```rust
#[derive(Clone, Debug, PartialEq)]
pub struct HirClass {
	pub name: EcoString,
	pub fields: Vec<EcoString>,
	pub methods: Vec<HirMethod>,
}

/// An inherent instance method → a JS class method. `this` in the body refers to
/// the receiver instance.
#[derive(Clone, Debug, PartialEq)]
pub struct HirMethod {
	pub name: EcoString,
	pub params: Vec<EcoString>,
	pub body: HirExpr,
}
```

Add to `enum HirExpr`:

```rust
	/// The method receiver — emits as the JS `this` keyword.
	This,
```

- [ ] **Step 2: Keep the tree compiling**

In `emit.rs::emit_expr`, add a temporary arm `HirExpr::This => unreachable!("emitted in Task 3")` (Task 3 replaces it). `emit_class` ignores `class.methods` for now. In `lower_hir.rs::lower_module`, add `methods: Vec::new()` to the `HirClass { … }` literal.

- [ ] **Step 3: Build**

Run: `cargo +nightly build -p nymph-codegen`
Expected: compiles.

- [ ] **Step 4: Commit**

```bash
cargo +nightly fmt -p nymph-hir -p nymph-codegen -p nymph-sema
jj commit -m "feat(hir): class methods (HirMethod) + This receiver node" -m "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>" crates/nymph-hir crates/nymph-codegen crates/nymph-sema
```

---

## Task 2: Lower `this` and collect struct methods

**Files:**
- Modify: `crates/nymph-sema/src/lower_hir.rs`
- Test: `crates/nymph-sema/tests/lower_hir.rs`

**Interfaces:**
- Consumes: `HirClass.methods`, `HirMethod`, `HirExpr::This` (Task 1); `ExprKind::This`; `Declaration::Impl { type_, members }`, `Type::Reference { name }`, `ImplMember::Func { meta, body }`, `StructInnerMember::Member`.

- [ ] **Step 1: Write the failing lowering test**

Add to `crates/nymph-sema/tests/lower_hir.rs`:

```rust
#[test]
fn lowers_struct_methods_and_this() {
	use nymph_hir::hir::HirExpr;
	let hir = lower(
		r#"
		struct Point(x: int, y: int)
		impl Point {
			func sum(): int = this.x + this.y
		}
		"#,
	);
	assert_eq!(hir.classes.len(), 1);
	let class = &hir.classes[0];
	assert_eq!(class.name, "Point");
	assert_eq!(class.methods.len(), 1);
	assert_eq!(class.methods[0].name, "sum");
	// The body `this.x + this.y` lowers with `This` receivers under the field access.
	let HirExpr::Binary { lhs, .. } = &class.methods[0].body else {
		panic!("expected a binary body, got {:?}", class.methods[0].body);
	};
	assert!(matches!(
		lhs.as_ref(),
		HirExpr::Field { recv, name } if matches!(recv.as_ref(), HirExpr::This) && name == "x"
	));
}
```

> Verify the inherent-impl surface parses via the passing sema test `crates/nymph-sema/tests/members.rs::top_level_inherent_impl`. Verify `StructInnerMember` and `ImplMember` shapes in `crates/nymph-ast/src/decl.rs`.

- [ ] **Step 2: Run; expect failure**

Run: `cargo +nightly test -p nymph-sema --test lower_hir lowers_struct_methods_and_this`
Expected: FAIL — `impl` blocks are ignored (no methods) and `ExprKind::This` hits the `other => panic!` catch-all.

- [ ] **Step 3: Lower `this`**

In `lower_hir.rs::lower_expr`, add an arm (before the `other =>` catch-all):

```rust
			ExprKind::This => HirExpr::This,
```

- [ ] **Step 4: Collect struct methods in `lower_module`**

Add a pre-pass that maps struct name → methods, gathered from top-level `impl` blocks and struct-inner members, then attach to each `HirClass`.

- Add a helper to lower one `ImplMember::Func` into a `HirMethod` (mirrors `lower_func`):

```rust
	fn lower_method(&self, meta: &FuncDeclaration, body: &Expr) -> HirMethod {
		HirMethod {
			name: meta.name.0.clone(),
			params: meta.params.iter().map(|p| param_name(&p.0.name)).collect(),
			body: self.lower_expr(body),
		}
	}
```

- Build the map before/while walking members. For each top-level `Declaration::Impl { type_, members, .. }` whose `type_.0` is `Type::Reference { name, .. }` (with no generic args in 4A), collect each `members` entry that is `ImplMember::Func { meta, body, .. }` into `methods_by_type[name.0]`. For a `Declaration::Struct`, also collect its `members` that are `StructInnerMember::Member(boxed)` where the inner `ImplMember` is `Func`. Then in the `Declaration::Struct` arm of `lower_module`, set `methods` from `methods_by_type` (drain/clone by name) plus the struct's own inner methods.

Concretely, restructure `lower_module` so it (1) first walks all members building `methods_by_type: FxHashMap<EcoString, Vec<HirMethod>>` (top-level impls) — but note `lower_method` needs `&self`, which is available — then (2) walks again to build funcs/classes/enums, and when building a `HirClass`, take `methods_by_type.remove(&name.0).unwrap_or_default()` and append the struct's own inner `Member` methods.

> `Declaration::Impl` targeting anything other than `Type::Reference` (a list/tuple/generic-with-args) is out of 4A scope: skip it (do not panic — other impls like `impl … for` and `impl mut` exist and will be handled later; silently skipping inherent impls on non-struct types is acceptable since their methods simply won't be emitted yet, and no current test exercises them). Inner `StructInnerMember` variants other than `Member`-with-`Func` (Namespace, Impl, ImplMut) are skipped in 4A.

- [ ] **Step 5: Run lowering test + sema suite**

Run: `cargo +nightly test -p nymph-sema`
Expected: PASS.

- [ ] **Step 6: fmt, clippy, commit**

```bash
cargo +nightly fmt -p nymph-sema && cargo +nightly clippy -p nymph-sema --all-targets
jj commit -m "feat(sema): lower this and collect inherent struct methods into HirClass" -m "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>" crates/nymph-sema
```

---

## Task 3: Emit class methods + `this`, run under Node

**Files:**
- Modify: `crates/nymph-codegen/src/emit.rs`
- Test: `crates/nymph-codegen/tests/run_node.rs`

**Interfaces:**
- Consumes: `HirClass.methods`, `HirMethod`, `HirExpr::This`.

- [ ] **Step 1: Write the failing Node tests**

```rust
#[test]
fn runs_struct_method_with_this() {
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
	let src = r#"
		struct Counter(n: int)
		impl Counter {
			func add(k: int): int = this.n + k
		}
		func bump(c: Counter): int = c.add(10)
	"#;
	assert_eq!(run(src, "bump(new Counter({ n: 5 }))"), "15");
}
```

- [ ] **Step 2: Run; expect failure**

Run: `cargo +nightly test -p nymph-codegen --test run_node runs_struct_method_with_this`
Expected: FAIL — the class has no `sum` method (methods not emitted) / `this` hits the Task 1 `unreachable!`.

- [ ] **Step 3: Emit `this`**

Replace the temporary arm in `emit_expr`:

```rust
			HirExpr::This => self.ast.expression_this(SPAN),
```

- [ ] **Step 4: Emit methods into the class body**

In `emit_class`, after building the constructor `ClassElement`, emit one method element per `class.methods` entry and push them all into the class body `elements`. Factor a helper that builds a `class_element_method_definition` from a name + params + body (mirror `emit_func`'s param/body handling and the constructor's `class_element_method_definition` call, but with `MethodDefinitionKind::Method` and the method's own `property_key_static_identifier(name)`):

```rust
	fn emit_method(&self, method: &HirMethod) -> ClassElement<'a> {
		// function body: `return <body>;` (or block-body, like emit_func)
		let mut body_stmts = self.ast.vec();
		match &method.body {
			HirExpr::Block { .. } => {
				let value = self.emit_value(&method.body);
				body_stmts.extend(value.stmts);
				body_stmts.push(self.ast.statement_return(SPAN, Some(value.expr)));
			}
			other => {
				let body_expr = self.emit_expr(other);
				body_stmts.push(self.ast.statement_return(SPAN, Some(body_expr)));
			}
		}
		let mut params = self.ast.vec();
		for param in &method.params {
			let pat = self
				.ast
				.binding_pattern_binding_identifier(SPAN, self.ast.allocator.alloc_str(param));
			params.push(self.ast.plain_formal_parameter(SPAN, pat));
		}
		let formal = self.ast.formal_parameters(
			SPAN,
			FormalParameterKind::FormalParameter,
			params,
			oxc::ast::NONE,
		);
		let fn_body = self.ast.function_body(SPAN, self.ast.vec(), body_stmts);
		let func = self.ast.alloc_function(
			SPAN,
			FunctionType::FunctionExpression,
			None,
			false,
			false,
			false,
			oxc::ast::NONE,
			oxc::ast::NONE,
			formal,
			oxc::ast::NONE,
			Some(fn_body),
		);
		self.ast.class_element_method_definition(
			SPAN,
			MethodDefinitionType::MethodDefinition,
			self.ast.vec(),
			self
				.ast
				.property_key_static_identifier(SPAN, self.ast.allocator.alloc_str(&method.name)),
			func,
			MethodDefinitionKind::Method,
			false,
			false,
			false,
			false,
			None,
		)
	}
```

Then in `emit_class`, after `elements.push(ctor);`, add `for method in &class.methods { elements.push(self.emit_method(method)); }`.

> The `emit_method` body-emission duplicates `emit_func`'s. If it reads cleanly, factor the shared "HIR expr → function body statements" into one helper used by both. Verify `MethodDefinitionKind::Method` and the builder arg list against oxc 0.139 (the constructor already uses this builder with `MethodDefinitionKind::Constructor`).

- [ ] **Step 5: Run execution tests**

Run: `cargo +nightly test -p nymph-codegen --test run_node`
Expected: PASS — `total(Point 3,4)`→7, `bump(Counter 5)`→15, plus all prior tests.

- [ ] **Step 6: Full workspace gate + fmt + commit**

```bash
cargo +nightly test && cargo +nightly clippy --all-targets
cargo +nightly fmt -p nymph-codegen
jj commit -m "feat(codegen): emit inherent struct methods as class methods; this keyword" -m "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>" crates/nymph-codegen
```

---

## Self-Review

**Spec coverage (design §"Slice 4 — Operators, methods, interfaces, ranges", methods subset):**
- Inherent instance methods on structs → JS class methods ✓
- `this` receiver ✓
- Method calls run (structural, pre-existing path) ✓
- Both top-level `impl` and struct-inner methods collected ✓

**Deferred, correctly (each its own follow-on):** operator overloading (needs checker Milestone-B operator/`DispatchKind` recording, then lowering emits native op vs method call); interface impls + dispatch; enum methods (ABI decision needed — enums aren't classes); namespaced/static methods; external companions; `mut` methods; ranges; `??`/`?`/`as`/`is`.

**Placeholder scan:** no "TBD". oxc 0.139 method-emission reuses the proven constructor path (`class_element_method_definition`) with a compile-to-verify note. AST reads (`Declaration::Impl`, `Type::Reference`, `StructInnerMember::Member`, `ImplMember::Func`) carry "verify against current code" notes. The method-collection restructuring of `lower_module` is described concretely (two-pass: build `methods_by_type`, then attach).

**Type consistency:** `HirClass { name, fields, methods }`; `HirMethod { name, params, body }`; `HirExpr::This`. Consistent across Tasks 1–3.

**Scope:** one coherent, Node-testable increment — struct instance methods, the foundation the rest of Slice 4 (operators desugar to method calls) builds on. Operators/interfaces/enums-methods/externals are separate follow-ons, and the checker is already ready for this subset (inherent method bodies type-check today).
