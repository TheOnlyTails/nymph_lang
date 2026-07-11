# Codegen Slice 2B (Structs & Field Access) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Compile Nymph `struct` declarations, struct construction, and field access to JS classes so struct values round-trip and print correct fields under Node.

**Architecture:** Extend the type-free HIR with class declarations and two expression nodes (`New`, `Field`). A pure structural lowering pass adds struct handling: struct decls → `HirClass`, construction calls → `New`, member access → `Field`. Construction is detected by a Lowerer pre-pass over the module's struct names (sound because lowering only runs on error-free programs, where struct and function names cannot collide). Codegen emits ES-class declarations, `new Class({…})`, and `recv.field`.

**Tech Stack:** Rust (nightly), oxc 0.139 (`AstBuilder` + `Codegen`), jj VCS, Node for execution tests.

## Global Constraints

- **Toolchain:** every cargo command uses `cargo +nightly` (the shell pins stable 1.96.0, which fails to build).
- **VCS is jj**, not git. Commit with `jj commit -m "line1" -m "line2" …` (multiple `-m` flags = paragraphs). NEVER use `$(cat <<EOF)` for messages — `cat` is aliased to `bat` and corrupts them with ANSI. Path-scope with trailing path args.
- **oxc is 0.139.** Its `AstBuilder` node-construction API is `#[deprecated]` (module-scoped `#![allow(deprecated)]` in `emit.rs`); builder signatures drift between patch versions — verify every builder call by compiling and adjust to what the compiler accepts.
- **Codegen stays type-free.** Lowering bakes every decision into concrete HIR node shapes; `emit.rs` never consults types or annotations.
- Commit message co-author trailer: `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`.

## Design Decisions (scope)

- **Structs → JS classes**, with an object-argument constructor that assigns each declared field:
  ```js
  class Point {
    constructor(fields) {
      this.x = fields.x;
      this.y = fields.y;
    }
  }
  ```
  Field names and order come from the AST struct declaration.
- **Construction — labeled args only in 2B.** `Point(x = 1, y = 2)` → `new Point({ x: 1, y: 2 })`. Positional construction (`Point(1, 2)`) needs field-order remapping and is **deferred** — lowering panics with a clear message.
- **Field defaults deferred.** `StructField.default` is ignored in 2B (add when a program needs it).
- **Field access** — a standalone `MemberAccess` (not the callee of a `Call`) lowers to `Field`. Method calls (`recv.method(…)`, i.e. `Call { func: MemberAccess }`) remain deferred to Slice 5.
- **No checker changes.** Construction is detected structurally in the Lowerer; nothing new is recorded during checking.

---

## Task 1: HIR class + New/Field nodes

**Files:**
- Modify: `crates/nymph-hir/src/hir.rs`
- Modify: `crates/nymph-codegen/src/emit.rs` (temporary `unreachable!` arms + ignore classes, to keep the crate compiling until Tasks 3)

**Interfaces:**
- Produces: `HirClass { name: EcoString, fields: Vec<EcoString> }`; `HirModule.classes: Vec<HirClass>`; `HirExpr::New { class: EcoString, fields: Vec<(EcoString, HirExpr)> }`; `HirExpr::Field { recv: Box<HirExpr>, name: EcoString }`.

- [ ] **Step 1: Add the class type and expr variants**

In `crates/nymph-hir/src/hir.rs`, add `classes` to `HirModule` and a `HirClass` struct:

```rust
#[derive(Clone, Debug, PartialEq)]
pub struct HirModule {
	pub funcs: Vec<HirFunc>,
	pub classes: Vec<HirClass>,
}

/// A `struct` declaration → a JS class. Fields are stored in declaration order;
/// the emitted constructor takes one object argument and assigns each field.
#[derive(Clone, Debug, PartialEq)]
pub struct HirClass {
	pub name: EcoString,
	pub fields: Vec<EcoString>,
}
```

Add to `enum HirExpr` (near `Array`/`MapGet`):

```rust
	/// Struct construction → `new <class>({ field: value, … })`.
	New {
		class: EcoString,
		fields: Vec<(EcoString, HirExpr)>,
	},
	/// Field access → `recv.name`.
	Field {
		recv: Box<HirExpr>,
		name: EcoString,
	},
```

- [ ] **Step 2: Keep `emit.rs` compiling**

`emit_module` constructs its statement list from `module.funcs`; leave it doing exactly that for now (classes are emitted in Task 3). In `emit_expr`, add a temporary arm after the collection arms:

```rust
			// Struct construction and field access are lowered in Task 2 but not
			// emitted until Task 3.
			HirExpr::New { .. } | HirExpr::Field { .. } => unreachable!("emitted in Task 3"),
```

- [ ] **Step 3: Build**

Run: `cargo +nightly build -p nymph-codegen`
Expected: compiles (every `HirModule { funcs }` literal must now also set `classes` — the lowering pass is the only constructor; fix it in Task 2. If Task 1 is committed alone, temporarily add `classes: Vec::new()` at the one construction site in `lower_hir.rs::lower_module` so the tree builds).

- [ ] **Step 4: Commit**

```bash
cargo +nightly fmt -p nymph-hir -p nymph-codegen
jj commit -m "feat(hir): class decls + New/Field expr nodes for structs" -m "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>" crates/nymph-hir crates/nymph-codegen
```

---

## Task 2: Lower struct decls, construction, and field access

**Files:**
- Modify: `crates/nymph-sema/src/lower_hir.rs`
- Test: `crates/nymph-sema/tests/lower_hir.rs`

**Interfaces:**
- Consumes: `HirClass`, `HirExpr::{New, Field}` (Task 1); `Declaration::Struct { name, fields, .. }`; `ExprKind::{Call, MemberAccess}`; `CallArg { value, name, spread }`.
- Produces: struct-aware `lower_module`; construction/field lowering in `lower_expr`.

- [ ] **Step 1: Write the failing lowering tests**

Add to `crates/nymph-sema/tests/lower_hir.rs`:

```rust
#[test]
fn lowers_struct_decl_and_construction() {
	let hir = lower(
		r#"
		struct Point { x: int, y: int }
		func origin(): Point = Point(x = 0, y = 0)
		"#,
	);
	// The struct becomes a class carrying its field names in order.
	assert_eq!(hir.classes.len(), 1);
	assert_eq!(hir.classes[0].name, "Point");
	assert_eq!(hir.classes[0].fields, vec!["x".to_string(), "y".to_string()]);

	// Construction lowers to a `New` naming the class, with labeled field values.
	let f = hir.funcs.iter().find(|f| f.name == "origin").expect("origin");
	let nymph_hir::hir::HirExpr::New { class, fields } = &f.body else {
		panic!("expected New, got {:?}", f.body);
	};
	assert_eq!(class, "Point");
	assert_eq!(fields.len(), 2);
	assert_eq!(fields[0].0, "x");
	assert_eq!(fields[1].0, "y");
}

#[test]
fn lowers_field_access() {
	let hir = lower(
		r#"
		struct Point { x: int, y: int }
		func get_x(p: Point): int = p.x
		"#,
	);
	let f = hir.funcs.iter().find(|f| f.name == "get_x").expect("get_x");
	let nymph_hir::hir::HirExpr::Field { recv, name } = &f.body else {
		panic!("expected Field, got {:?}", f.body);
	};
	assert_eq!(name, "x");
	assert!(matches!(recv.as_ref(), nymph_hir::hir::HirExpr::Local(n) if n == "p"));
}
```

- [ ] **Step 2: Run; expect failure**

Run: `cargo +nightly test -p nymph-sema --test lower_hir lowers_struct_decl_and_construction`
Expected: FAIL — `lower_module` ignores structs (no classes) and `lower_expr` panics on the construction `Call` / `MemberAccess`.

- [ ] **Step 3: Collect struct names and lower struct decls**

In `crates/nymph-sema/src/lower_hir.rs`, rework `lower_module` to (a) collect struct names into the `Lowerer` and (b) emit a `HirClass` per struct. Add a `struct_names: FxHashSet<EcoString>` field to `Lowerer` (import `rustc_hash::FxHashSet`; it is already a workspace dep of nymph-sema). Because `Lowerer`'s methods take `&self`, build the set before constructing the walking state:

```rust
pub fn lower_hir(module: &Module, checked: &Checked) -> HirModule {
	let struct_names = module
		.members
		.iter()
		.filter_map(|decl| match decl {
			Declaration::Struct { name, .. } => Some(name.0.clone()),
			_ => None,
		})
		.collect();
	let lowerer = Lowerer {
		annotations: &checked.annotations,
		interner: &checked.interner,
		struct_names,
	};
	lowerer.lower_module(module)
}
```

Add the field to the struct:

```rust
struct Lowerer<'a> {
	annotations: &'a Annotations,
	interner: &'a Interner,
	struct_names: rustc_hash::FxHashSet<ecow::EcoString>,
}
```

Rewrite `lower_module` to gather funcs and classes:

```rust
	fn lower_module(&self, module: &Module) -> HirModule {
		let mut funcs = Vec::new();
		let mut classes = Vec::new();
		for decl in &module.members {
			match decl {
				Declaration::Func { meta, body, .. } => funcs.push(self.lower_func(meta, body)),
				Declaration::Struct { name, fields, .. } => classes.push(HirClass {
					name: name.0.clone(),
					fields: fields.iter().map(|f| f.0.name.0.clone()).collect(),
				}),
				_ => {}
			}
		}
		HirModule { funcs, classes }
	}
```

Add the imports: `HirClass` to the `nymph_hir::hir` use list, and `StructField` is reached via `f.0.name.0` (no new import — `fields` is `Vec<Spanned<StructField>>` and `Spanned` is `(T, Span)`, so `f.0.name.0` is the field's `EcoString`). Verify `StructField`'s `name` field is an `Ident` (`crates/nymph-ast/src/decl.rs`): `Ident` is `Spanned<EcoString>`-like, so `f.0.name.0` yields the `EcoString`. Adjust the exact projection to what compiles.

- [ ] **Step 4: Lower construction and field access in `lower_expr`**

Replace the existing `ExprKind::Call` arm so a call whose callee is a known struct name lowers to `New`; otherwise keep the existing call lowering:

```rust
			ExprKind::Call { func, args, .. } => {
				if let ExprKind::Identifier(name) = &func.kind
					&& self.struct_names.contains(&name.0)
				{
					// Struct construction. 2B supports labeled args only.
					let fields = args
						.iter()
						.map(|a| {
							let label = a
								.0
								.name
								.as_ref()
								.unwrap_or_else(|| {
									panic!("slice-2b struct construction requires labeled fields")
								});
							(label.0.clone(), self.lower_expr(&a.0.value))
						})
						.collect();
					HirExpr::New {
						class: name.0.clone(),
						fields,
					}
				} else {
					HirExpr::Call {
						callee: Box::new(self.lower_expr(func)),
						args: args.iter().map(|a| self.lower_expr(&a.0.value)).collect(),
					}
				}
			}
```

Add a `MemberAccess` arm (place it near `IndexAccess`):

```rust
			ExprKind::MemberAccess { parent, member, .. } => HirExpr::Field {
				recv: Box::new(self.lower_expr(parent)),
				name: member.0.clone(),
			},
```

> Verify `member` is an `Ident` (so `member.0` is the `EcoString`) and that `CallArg.name` is `Option<Ident>` (so `label.0` is the `EcoString`). Both per `crates/nymph-ast/src/expr.rs`. Adjust projections to compile.

- [ ] **Step 5: Run the lowering tests**

Run: `cargo +nightly test -p nymph-sema --test lower_hir`
Expected: PASS — all prior lower_hir tests plus the two new ones.

- [ ] **Step 6: Sema suite + fmt + clippy**

Run: `cargo +nightly test -p nymph-sema && cargo +nightly fmt -p nymph-sema && cargo +nightly clippy -p nymph-sema --all-targets`
Expected: green; no new warnings in `nymph-sema` (ignore pre-existing warnings in crates you don't own).

- [ ] **Step 7: Commit**

```bash
jj commit -m "feat(sema): lower struct decls to classes, construction to New, field access to Field" -m "Construction is detected via a Lowerer pre-pass over module struct names (sound on error-free programs). Labeled construction args only; positional deferred." -m "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>" crates/nymph-sema
```

---

## Task 3: Emit classes + construction + field access, run under Node

**Files:**
- Modify: `crates/nymph-codegen/src/emit.rs`
- Test: `crates/nymph-codegen/tests/run_node.rs`

**Interfaces:**
- Consumes: `HirModule.classes`, `HirClass`, `HirExpr::{New, Field}`.

- [ ] **Step 1: Write the failing Node-execution tests**

Add to `crates/nymph-codegen/tests/run_node.rs`:

```rust
#[test]
fn runs_struct_construction_and_field() {
	let src = r#"
		struct Point { x: int, y: int }
		func make(): Point = Point(x = 3, y = 4)
	"#;
	// Construct in JS and read a field back.
	assert_eq!(run(src, "make().y"), "4");
}

#[test]
fn runs_struct_field_through_param() {
	let src = r#"
		struct Point { x: int, y: int }
		func sum(p: Point): int = p.x + p.y
	"#;
	assert_eq!(run(src, "sum(new Point({ x: 10, y: 20 }))"), "30");
}
```

- [ ] **Step 2: Run; expect failure**

Run: `cargo +nightly test -p nymph-codegen --test run_node runs_struct_construction_and_field`
Expected: FAIL — `emit_module` emits no classes (so `Point` is undefined / `new` hits the `unreachable!`).

- [ ] **Step 3: Emit class declarations**

In `emit.rs`, emit each class before the functions. In `emit_module`, after building the function statements, prepend class statements:

```rust
	pub fn emit_module(&self, module: &HirModule) -> String {
		let mut stmts = self.ast.vec();
		for class in &module.classes {
			stmts.push(self.emit_class(class));
		}
		for func in &module.funcs {
			stmts.push(self.emit_func(func));
		}
		// … existing program build …
	}
```

Add `emit_class`, building `class <name> { constructor(fields) { this.<f> = fields.<f>; … } }`. Model the member expressions on the existing `Field`/`assign_target` helpers and verify every builder name against oxc 0.139 by compiling:

```rust
	fn emit_class(&self, class: &HirClass) -> Statement<'a> {
		// constructor body: `this.<f> = fields.<f>;` for each field
		let mut body = self.ast.vec();
		for field in &class.fields {
			let field_str = self.ast.allocator.alloc_str(field);
			// this.<field>
			let this_expr = self.ast.expression_this(SPAN);
			let target = AssignmentTarget::from(SimpleAssignmentTarget::from(
				self.ast.member_expression_static(
					SPAN,
					this_expr,
					self.ast.identifier_name(SPAN, field_str),
					false,
				),
			));
			// fields.<field>
			let fields_ident = self.ast.expression_identifier(SPAN, "fields");
			let value = Expression::from(self.ast.member_expression_static(
				SPAN,
				fields_ident,
				self.ast.identifier_name(SPAN, field_str),
				false,
			));
			let assign =
				self
					.ast
					.expression_assignment(SPAN, AssignmentOperator::Assign, target, value);
			body.push(self.ast.statement_expression(SPAN, assign));
		}
		// constructor(fields) { <body> }
		let mut params = self.ast.vec();
		let fields_pat = self.ast.binding_pattern_binding_identifier(SPAN, "fields");
		params.push(self.ast.plain_formal_parameter(SPAN, fields_pat));
		let ctor_params = self.ast.formal_parameters(
			SPAN,
			FormalParameterKind::FormalParameter,
			params,
			oxc::ast::NONE,
		);
		let ctor_body = self.ast.function_body(SPAN, self.ast.vec(), body);
		let ctor_fn = self.ast.alloc_function(
			SPAN,
			FunctionType::FunctionExpression,
			None,
			false,
			false,
			false,
			oxc::ast::NONE,
			oxc::ast::NONE,
			ctor_params,
			oxc::ast::NONE,
			Some(ctor_body),
		);
		let ctor_key = self.ast.property_key_static_identifier(SPAN, "constructor");
		let ctor = self.ast.class_element_method_definition(
			SPAN,
			MethodDefinitionType::MethodDefinition,
			self.ast.vec(),
			ctor_key,
			ctor_fn,
			MethodDefinitionKind::Constructor,
			false,
			false,
			false,
			false,
			oxc::ast::NONE,
		);
		let mut elements = self.ast.vec();
		elements.push(ctor);
		let class_body = self.ast.class_body(SPAN, elements);
		let name_id = self
			.ast
			.binding_identifier(SPAN, self.ast.allocator.alloc_str(&class.name));
		let class = self.ast.alloc_class(
			SPAN,
			ClassType::ClassDeclaration,
			self.ast.vec(),
			Some(name_id),
			oxc::ast::NONE,
			None,
			oxc::ast::NONE,
			None,
			oxc::ast::NONE,
			class_body,
			false,
			false,
		);
		Statement::ClassDeclaration(class)
	}
```

> The oxc 0.139 builder names above (`expression_this`, `member_expression_static`, `class_element_method_definition`, `alloc_class`, `class_body`, `property_key_static_identifier`, and the `SimpleAssignmentTarget`/`AssignmentTarget` conversions) are best-effort shapes. Their exact argument lists WILL differ — build repeatedly and let the compiler drive the signatures. If a class-method builder proves awkward, an equivalent fallback is to emit the constructor as an assigned function or to emit the class via a small hand-built node; keep the emitted JS to `class <Name> { constructor(fields) { this.<f> = fields.<f>; } }`.

- [ ] **Step 4: Emit `New` and `Field`**

Replace the temporary `unreachable!` arm from Task 1 Step 2 with:

```rust
			// new <class>({ field: value, … })
			HirExpr::New { class, fields } => {
				let mut props = self.ast.vec();
				for (name, value) in fields {
					let key = self
						.ast
						.property_key_static_identifier(SPAN, self.ast.allocator.alloc_str(name));
					let val = self.emit_expr(value);
					props.push(ObjectPropertyKind::ObjectProperty(self.ast.alloc_object_property(
						SPAN,
						PropertyKind::Init,
						key,
						val,
						false,
						false,
						false,
					)));
				}
				let obj = self.ast.expression_object(SPAN, props);
				let callee = self
					.ast
					.expression_identifier(SPAN, self.ast.allocator.alloc_str(class));
				let mut args = self.ast.vec();
				args.push(Argument::from(obj));
				self.ast.expression_new(SPAN, callee, oxc::ast::NONE, args)
			}
			// recv.name
			HirExpr::Field { recv, name } => {
				let object = self.emit_expr(recv);
				Expression::from(self.ast.member_expression_static(
					SPAN,
					object,
					self.ast.identifier_name(SPAN, self.ast.allocator.alloc_str(name)),
					false,
				))
			}
```

> Verify `expression_object`, `alloc_object_property`, `ObjectPropertyKind`, `PropertyKind`, and `member_expression_static` against oxc 0.139 by compiling; adjust to the accepted signatures. The `Field` emitter mirrors the `MapGet` receiver-member shape already in `emit.rs`.

- [ ] **Step 5: Run the execution tests**

Run: `cargo +nightly test -p nymph-codegen --test run_node`
Expected: PASS — `make().y`→`4`, `sum(new Point({x:10,y:20}))`→`30`, plus all Slice 1/2A tests.

- [ ] **Step 6: Full workspace gate**

Run: `cargo +nightly test && cargo +nightly clippy --all-targets`
Expected: green / clean (ignore pre-existing warnings in `nymph-diagnostics` and the error-code crates you don't own).

- [ ] **Step 7: Format, commit**

```bash
cargo +nightly fmt -p nymph-codegen
jj commit -m "feat(codegen): emit struct classes, construction (new Class({…})), and field access; run under Node" -m "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>" crates/nymph-codegen
```

---

## Self-Review

**Spec coverage (against the design's "Slice 2 — Data types & the value ABI", structs subset):**
- Structs → JS classes ✓ (Task 3)
- Struct construction → `new Class({…})` ✓ (Tasks 2–3)
- Field access → `recv.field` ✓ (Tasks 2–3)
- Round-trips under Node ✓ (Task 3 tests)

**Deferred, correctly:** positional construction args (need field-order remap); field defaults; instance methods / method calls (`Call { func: MemberAccess }`, Slice 5); enums + the Symbol tag ABI + `equality.ts` (Slice 2C); defensive `Copy` (2C). Each is a clean follow-on.

**Placeholder scan:** No "TBD"/"handle edge cases". oxc 0.139 builder names are given as reference shapes with an explicit "verify by compiling" instruction (unavoidable — only the compiler pins the signatures). AST projections (`f.0.name.0`, `member.0`, `CallArg.name`) carry "verify against current code" notes because they are read, not guessed.

**Type consistency:** `HirModule { funcs, classes }`; `HirClass { name, fields: Vec<EcoString> }`; `HirExpr::New { class: EcoString, fields: Vec<(EcoString, HirExpr)> }`; `HirExpr::Field { recv: Box<HirExpr>, name: EcoString }`; `lower_hir(&Module, &Checked)` unchanged. Names/shapes match across Tasks 1–3.

**Scope:** One coherent, Node-testable increment — the struct value form (declaration, construction, field access). Enums/copy and methods are separate follow-on plans.
