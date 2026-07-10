# Nymph Codegen — Slice 1 (Core Expressions & Functions) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Emit runnable JavaScript for the scalar/control-flow core of Nymph — literals, identifiers, `let`/`mut`, free functions, calls, primitive arithmetic/comparison/logical operators, blocks, and `if`/`while` in both statement and value position — so hand-written arithmetic and control-flow programs run under Node and print correct results.

**Architecture:** Introduces the first `nymph-hir` HIR node types (a **type-free** subset — see Design Decisions), a **structural** `lower_hir` pass in `nymph-sema` (AST → HIR, no annotation/type consumption yet), and a new `nymph-codegen` crate that emits JS from HIR via `oxc`'s `AstBuilder` + `Codegen`. An end-to-end `compile(source) -> String` ties parse → check → lower → emit together, verified by executing the emitted JS under Node.

**Tech Stack:** Rust (edition 2024, nightly), `oxc` 0.138 (`codegen` feature, already a workspace dep), `ecow`, the existing `nymph-ast`/`nymph-syntax`/`nymph-sema`/`nymph-hir` crates, and Node.js (v26, at `node` on PATH) for execution tests.

## Global Constraints

- **Toolchain:** every cargo command MUST be prefixed `cargo +nightly` (the shell pins stable via `RUSTUP_TOOLCHAIN`, which breaks the build otherwise).
- **Edition:** 2024, inherited from `[workspace.package]`.
- **oxc version is 0.138.0** (`oxc = { version = "0.138.0", features = ["codegen"] }` in the root `Cargo.toml`). The reference transpiler at `reference/compiler/src/transpiler/emit.rs` uses oxc **0.123** — its `AstBuilder` method *shapes* are a close guide, but exact signatures may have shifted; the Task 2 spike exists to pin the 0.138 API before building out. Never assume a builder signature without compiling.
- **Formatting/lints:** finish each task `cargo +nightly fmt` clean and `cargo +nightly clippy --all-targets` clean (the tree is clippy-clean today).
- **Emitted JS ABI (must stay compatible with later slices):** enums `{ [TAG]: sym, ...fields }`, structs classes, lists/tuples arrays, maps `Map` — none of these appear in Slice 1, but do not emit anything that conflicts with them.
- **VCS is Jujutsu (jj), not git.** In subagent-driven execution the controller owns commits (`jj commit`); implementer subagents do NOT run git/jj and skip the plan's "Commit" steps.
- **Node execution:** `node` (v26+) is on PATH. Execution tests write emitted JS to a temp file and run it with `node`, asserting on stdout.

## Design Decisions (locked — deviations from the general spec, YAGNI-driven)

1. **Slice 1 HIR is type-free.** JS has one `number` type, so `int`/`uint`/`float` all emit identically and int-literal widening is invisible; primitive operators map 1:1 to JS operators (`int/int → float`, and JS `/` already yields float, so no truncation). Therefore Slice 1 codegen needs no type info, HIR nodes carry no `ty`, and lowering consults **neither** the annotations **nor** the interner. Type-carrying HIR fields, annotation consumption, and threading the `Interner` into `Checked`/lowering are deferred to Slice 2 (value-copy is the first feature that must interpret a `Ty`). *(The whole-branch review's "interner in `Checked`" follow-up lands there, not here.)*
2. **Binary operators lower to a native-JS `HirExpr::Binary`.** Slice 1 test programs use primitive operands only, which the checker resolves via its built-in fast-path (`DispatchKind::BuiltinEager` → native JS operator). Operator-overload dispatch to interface impls (ADT operands, `UserImpl`, external `.ts` calls) is Slice 4; until then lowering treats every binary operator as native JS. `&&`/`||` still emit as JS `&&`/`||` (which short-circuit natively — matching Nymph's built-in default).
3. **`let` → JS `const`, `mut` → JS `let`.** Nymph bindings are immutable by default.
4. **Control-flow-as-value uses result-temporary hoisting**, mirroring the reference emitter's `JsValue` split (leading statements + a final expression; wrap in an IIFE only when there are leading statements). `if`/`while`/`block` in statement position emit as plain JS statements.

---

## File Structure

- `crates/nymph-hir/src/hir.rs` — **new**: the HIR node types (`HirModule`, `HirFunc`, `HirExpr`, `HirStmt`, `BinOp`). Pure data, no logic.
- `crates/nymph-hir/src/lib.rs` — add `pub mod hir;` + re-exports.
- `crates/nymph-codegen/` — **new crate**: `Cargo.toml`, `src/lib.rs` (`emit(&HirModule) -> String`), `src/emit.rs` (the oxc `Emitter`).
- `crates/nymph-sema/src/lower_hir.rs` — **new**: `lower_hir(&Module) -> HirModule`, the structural AST→HIR pass. (Distinct from the existing `lower.rs`, which lowers surface *types*.)
- `crates/nymph-sema/src/lib.rs` — add `mod lower_hir;` + `pub use lower_hir::lower_hir;`.
- `crates/nymph-codegen/tests/` — lowering-independent emit snapshot tests and Node-execution tests.
- `Cargo.toml` (workspace) — uncomment `crates/nymph-codegen` in `members`; it already has a `[workspace.dependencies]` line.

---

## Task 1: HIR node types for the scalar/control-flow core

**Files:**
- Create: `crates/nymph-hir/src/hir.rs`
- Modify: `crates/nymph-hir/src/lib.rs`

**Interfaces:**
- Produces (all `#[derive(Clone, Debug, PartialEq)]`):
  - `HirModule { pub funcs: Vec<HirFunc> }`
  - `HirFunc { pub name: EcoString, pub params: Vec<EcoString>, pub body: HirExpr }`
  - `HirStmt` enum: `Let { name: EcoString, mutable: bool, value: HirExpr }`, `Expr(HirExpr)`
  - `HirExpr` enum:
    - `Num(f64)` — every numeric literal (int/uint/float) as a JS number
    - `Str(EcoString)`, `Bool(bool)`, `Char(char)`
    - `Local(EcoString)` — identifier / parameter reference
    - `Call { callee: Box<HirExpr>, args: Vec<HirExpr> }`
    - `Binary { op: BinOp, lhs: Box<HirExpr>, rhs: Box<HirExpr> }`
    - `Unary { op: UnOp, operand: Box<HirExpr> }`
    - `Block { stmts: Vec<HirStmt>, tail: Option<Box<HirExpr>> }`
    - `If { cond: Box<HirExpr>, then: Box<HirExpr>, otherwise: Option<Box<HirExpr>> }`
    - `While { cond: Box<HirExpr>, body: Box<HirExpr> }`
  - `BinOp` enum: `Add, Sub, Mul, Div, Rem, Pow, Eq, Ne, Lt, Le, Gt, Ge, And, Or, BitAnd, BitOr, BitXor, Shl, Shr`
  - `UnOp` enum: `Neg, Not`

- [ ] **Step 1: Write the HIR types**

Create `crates/nymph-hir/src/hir.rs`:

```rust
//! The mid-level typed IR consumed by code generation. Slice 1 covers the
//! scalar/control-flow core and is deliberately *type-free*: JS has a single
//! `number` type and primitive operators map 1:1 to JS operators, so no type
//! information is needed to emit correct JS. Type-carrying fields arrive in later
//! slices, where value-copy and operator-overload dispatch first need them.

use ecow::EcoString;

#[derive(Clone, Debug, PartialEq)]
pub struct HirModule {
    pub funcs: Vec<HirFunc>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HirFunc {
    pub name: EcoString,
    pub params: Vec<EcoString>,
    pub body: HirExpr,
}

#[derive(Clone, Debug, PartialEq)]
pub enum HirStmt {
    /// `let`/`mut` binding. `mutable` selects JS `let` vs `const`.
    Let {
        name: EcoString,
        mutable: bool,
        value: HirExpr,
    },
    /// A bare expression evaluated for its effect.
    Expr(HirExpr),
}

#[derive(Clone, Debug, PartialEq)]
pub enum HirExpr {
    /// Any numeric literal (int/uint/float) — all are JS `number`.
    Num(f64),
    Str(EcoString),
    Bool(bool),
    Char(char),
    /// An identifier or parameter reference.
    Local(EcoString),
    Call {
        callee: Box<HirExpr>,
        args: Vec<HirExpr>,
    },
    Binary {
        op: BinOp,
        lhs: Box<HirExpr>,
        rhs: Box<HirExpr>,
    },
    Unary {
        op: UnOp,
        operand: Box<HirExpr>,
    },
    /// A block: statements then an optional trailing expression (the block's value).
    Block {
        stmts: Vec<HirStmt>,
        tail: Option<Box<HirExpr>>,
    },
    If {
        cond: Box<HirExpr>,
        then: Box<HirExpr>,
        otherwise: Option<Box<HirExpr>>,
    },
    While {
        cond: Box<HirExpr>,
        body: Box<HirExpr>,
    },
}

/// Binary operators that map directly to a JS operator (primitive fast-path).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add, Sub, Mul, Div, Rem, Pow,
    Eq, Ne, Lt, Le, Gt, Ge,
    And, Or,
    BitAnd, BitOr, BitXor, Shl, Shr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
}
```

- [ ] **Step 2: Export the module**

In `crates/nymph-hir/src/lib.rs`, add after the existing `pub mod` lines:

```rust
pub mod hir;
```

- [ ] **Step 3: Build**

Run: `cargo +nightly build -p nymph-hir`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/nymph-hir
git commit -m "feat(hir): scalar/control-flow HIR node types (slice 1)"
```

---

## Task 2: `nymph-codegen` crate + oxc 0.138 pipeline spike

**Purpose:** Stand up the new crate and pin the oxc-0.138 `AstBuilder`/`Codegen` API on a trivial hand-built `HirModule` before building the full emitter, so API churn is caught immediately.

**Files:**
- Create: `crates/nymph-codegen/Cargo.toml`, `crates/nymph-codegen/src/lib.rs`, `crates/nymph-codegen/src/emit.rs`
- Modify: root `Cargo.toml` (uncomment the `crates/nymph-codegen` member)
- Test: `crates/nymph-codegen/tests/emit.rs`

**Interfaces:**
- Consumes: `nymph_hir::hir::{HirModule, HirFunc, HirExpr}`.
- Produces: `nymph_codegen::emit(module: &HirModule) -> String`.

- [ ] **Step 1: Register the crate**

In the root `Cargo.toml` `members`, uncomment `"crates/nymph-codegen",`. Confirm `[workspace.dependencies]` has `nymph-codegen = { path = "crates/nymph-codegen" }` (it does).

Create `crates/nymph-codegen/Cargo.toml`:

```toml
[package]
name = "nymph-codegen"
description = "JavaScript code generation from the Nymph HIR."
version.workspace = true
edition.workspace = true
license.workspace = true
homepage.workspace = true
repository.workspace = true
authors.workspace = true

[dependencies]
nymph-hir = { workspace = true }
oxc = { workspace = true }
ecow = { workspace = true }
```

- [ ] **Step 2: Write the failing spike test**

Create `crates/nymph-codegen/tests/emit.rs`:

```rust
use nymph_codegen::emit;
use nymph_hir::hir::{HirExpr, HirFunc, HirModule};

#[test]
fn emits_a_function_returning_a_number() {
    let module = HirModule {
        funcs: vec![HirFunc {
            name: "answer".into(),
            params: vec![],
            body: HirExpr::Num(42.0),
        }],
    };
    let js = emit(&module);
    // A single-expression body becomes an arrow-style function returning the value.
    assert!(js.contains("answer"), "function name present: {js}");
    assert!(js.contains("42"), "literal present: {js}");
}
```

- [ ] **Step 3: Run it; expect a compile failure**

Run: `cargo +nightly test -p nymph-codegen`
Expected: FAIL — `emit` does not exist yet.

- [ ] **Step 4: Write the minimal emitter (pins the oxc 0.138 API)**

Create `crates/nymph-codegen/src/lib.rs`:

```rust
//! JavaScript code generation from the Nymph HIR, via oxc's AST builder + codegen.

mod emit;

use nymph_hir::hir::HirModule;

/// Emit an ES module string for `module`.
pub fn emit(module: &HirModule) -> String {
    emit::Emitter::new().emit_module(module)
}
```

Create `crates/nymph-codegen/src/emit.rs`. Model the oxc calls on `reference/compiler/src/transpiler/{mod.rs,emit.rs}` (imports: `oxc::allocator::Allocator`, `oxc::ast::{AstBuilder, ast::*}`, `oxc::span::SPAN`, `oxc::codegen::Codegen`). The reference uses oxc 0.123; **compile against 0.138 and adjust any builder signature the compiler rejects** (e.g. `expression_numeric_literal`, `function`, `function_body`, `formal_parameters`, `statement_return`, `program`, and the `Codegen::new().build(&program).code` call — verify each). Slice-1 minimal version: emit each `HirFunc` as a top-level `function name(params) { return <body-expr>; }`, and for this spike support only `HirExpr::Num`:

```rust
use oxc::{
    allocator::Allocator,
    ast::{AstBuilder, ast::*},
    codegen::Codegen,
    span::SPAN,
};

use nymph_hir::hir::{HirExpr, HirFunc, HirModule};

pub struct Emitter<'a> {
    ast: AstBuilder<'a>,
    #[allow(dead_code)]
    alloc: &'a Allocator,
}

impl Default for Emitter<'_> {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a> Emitter<'a> {
    pub fn new() -> Emitter<'static> {
        // Leak an allocator for the lifetime of the emit call; the returned String
        // outlives it. (A slice-1 simplification; a later slice can thread an
        // externally-owned Allocator if allocation pressure matters.)
        let alloc: &'static Allocator = Box::leak(Box::new(Allocator::default()));
        Emitter {
            ast: AstBuilder::new(alloc),
            alloc,
        }
    }

    pub fn emit_module(&self, module: &HirModule) -> String {
        let mut stmts = self.ast.vec();
        for func in &module.funcs {
            stmts.push(self.emit_func(func));
        }
        let program = self.ast.program(
            SPAN,
            SourceType::mjs(),
            "",
            self.ast.vec(),
            None,
            self.ast.vec(),
            stmts,
        );
        Codegen::new().build(&program).code
    }

    fn emit_func(&self, func: &HirFunc) -> Statement<'a> {
        // function <name>(<params>) { return <body>; }
        let body_expr = self.emit_expr(&func.body);
        let mut body_stmts = self.ast.vec();
        body_stmts.push(self.ast.statement_return(SPAN, Some(body_expr)));
        let params = self.ast.formal_parameters(
            SPAN,
            FormalParameterKind::FormalParameter,
            self.ast.vec(),
            oxc::ast::NONE,
        );
        let fn_body = self.ast.function_body(SPAN, self.ast.vec(), body_stmts);
        // Verify this `function` builder signature against oxc 0.138.
        let function = self.ast.alloc_function(
            SPAN,
            FunctionType::FunctionDeclaration,
            Some(self.ast.identifier_name(SPAN, self.ast.allocator.alloc_str(&func.name))),
            false,
            false,
            false,
            oxc::ast::NONE,
            oxc::ast::NONE,
            params,
            oxc::ast::NONE,
            Some(fn_body),
        );
        Statement::FunctionDeclaration(function)
    }

    fn emit_expr(&self, expr: &HirExpr) -> Expression<'a> {
        match expr {
            HirExpr::Num(value) => {
                self.ast
                    .expression_numeric_literal(SPAN, *value, None, NumberBase::Decimal)
            }
            _ => unreachable!("only Num is supported in the slice-1 spike"),
        }
    }
}
```

> The exact oxc builder argument lists above are the reference-0.123 shapes; the implementer MUST reconcile each with 0.138 (argument counts/order shift between oxc minors). The gate is that the crate compiles and the spike test passes.

- [ ] **Step 5: Build until the oxc API matches, then run the test**

Run: `cargo +nightly test -p nymph-codegen`
Expected: PASS — `emit(module)` returns a string containing `answer` and `42` (e.g. `function answer() { return 42; }`).

- [ ] **Step 6: Format, clippy, commit**

```bash
cargo +nightly fmt && cargo +nightly clippy -p nymph-codegen --all-targets
git add crates/nymph-codegen Cargo.toml Cargo.lock
git commit -m "feat(codegen): nymph-codegen crate + oxc 0.138 pipeline spike"
```

---

## Task 3: Emit scalar expressions and multi-param functions

**Files:**
- Modify: `crates/nymph-codegen/src/emit.rs`
- Test: `crates/nymph-codegen/tests/emit.rs`

**Interfaces:**
- Consumes: all `HirExpr` scalar/operator variants from Task 1.
- Produces: `Emitter::emit_expr` handles `Num`, `Str`, `Bool`, `Char`, `Local`, `Binary`, `Unary`, `Call`; `emit_func` emits declared parameters.

- [ ] **Step 1: Write failing emit tests**

Add to `crates/nymph-codegen/tests/emit.rs`:

```rust
use nymph_hir::hir::{BinOp, HirStmt, UnOp};

#[test]
fn emits_arithmetic_and_params() {
    // function add(a, b) { return a + b * 2; }
    let module = HirModule {
        funcs: vec![HirFunc {
            name: "add".into(),
            params: vec!["a".into(), "b".into()],
            body: HirExpr::Binary {
                op: BinOp::Add,
                lhs: Box::new(HirExpr::Local("a".into())),
                rhs: Box::new(HirExpr::Binary {
                    op: BinOp::Mul,
                    lhs: Box::new(HirExpr::Local("b".into())),
                    rhs: Box::new(HirExpr::Num(2.0)),
                }),
            },
        }],
    };
    let js = emit(&module);
    assert!(js.contains("function add(a, b)"), "{js}");
    assert!(js.contains("a + b * 2"), "{js}");
}

#[test]
fn emits_call_and_string() {
    // function greet() { return log("hi"); }
    let module = HirModule {
        funcs: vec![HirFunc {
            name: "greet".into(),
            params: vec![],
            body: HirExpr::Call {
                callee: Box::new(HirExpr::Local("log".into())),
                args: vec![HirExpr::Str("hi".into())],
            },
        }],
    };
    let js = emit(&module);
    assert!(js.contains("log('hi')") || js.contains("log(\"hi\")"), "{js}");
}
```

- [ ] **Step 2: Run; expect failure**

Run: `cargo +nightly test -p nymph-codegen emits_arithmetic_and_params`
Expected: FAIL — `unreachable!` panic (only `Num` handled).

- [ ] **Step 3: Implement the scalar/operator/call emitters**

In `crates/nymph-codegen/src/emit.rs`, add a `BinOp`→JS operator mapping and expand `emit_expr`. Replace the `emit_expr` match with:

```rust
fn emit_expr(&self, expr: &HirExpr) -> Expression<'a> {
    match expr {
        HirExpr::Num(value) => {
            self.ast
                .expression_numeric_literal(SPAN, *value, None, NumberBase::Decimal)
        }
        HirExpr::Str(s) => {
            self.ast
                .expression_string_literal(SPAN, self.ast.allocator.alloc_str(s), None)
        }
        HirExpr::Bool(b) => self.ast.expression_boolean_literal(SPAN, *b),
        HirExpr::Char(c) => {
            // A Nymph char is a single-character JS string.
            let s = self.ast.allocator.alloc_str(&c.to_string());
            self.ast.expression_string_literal(SPAN, s, None)
        }
        HirExpr::Local(name) => self
            .ast
            .expression_identifier(SPAN, self.ast.allocator.alloc_str(name)),
        HirExpr::Binary { op, lhs, rhs } => {
            let left = self.emit_expr(lhs);
            let right = self.emit_expr(rhs);
            self.emit_binary(*op, left, right)
        }
        HirExpr::Unary { op, operand } => {
            let inner = self.emit_expr(operand);
            let operator = match op {
                UnOp::Neg => UnaryOperator::UnaryNegation,
                UnOp::Not => UnaryOperator::LogicalNot,
            };
            self.ast.expression_unary(SPAN, operator, inner)
        }
        HirExpr::Call { callee, args } => {
            let callee = self.emit_expr(callee);
            let mut arguments = self.ast.vec();
            for arg in args {
                arguments.push(Argument::from(self.emit_expr(arg)));
            }
            self.ast
                .expression_call(SPAN, callee, oxc::ast::NONE, arguments, false)
        }
        HirExpr::Block { .. } | HirExpr::If { .. } | HirExpr::While { .. } => {
            unreachable!("control-flow expressions are handled in Task 5/6")
        }
    }
}

fn emit_binary(
    &self,
    op: nymph_hir::hir::BinOp,
    left: Expression<'a>,
    right: Expression<'a>,
) -> Expression<'a> {
    use nymph_hir::hir::BinOp;
    // Logical operators are a distinct oxc node from binary operators.
    if let BinOp::And | BinOp::Or = op {
        let operator = if op == BinOp::And {
            LogicalOperator::And
        } else {
            LogicalOperator::Or
        };
        return self.ast.expression_logical(SPAN, left, operator, right);
    }
    let operator = match op {
        BinOp::Add => BinaryOperator::Addition,
        BinOp::Sub => BinaryOperator::Subtraction,
        BinOp::Mul => BinaryOperator::Multiplication,
        BinOp::Div => BinaryOperator::Division,
        BinOp::Rem => BinaryOperator::Remainder,
        BinOp::Pow => BinaryOperator::Exponential,
        BinOp::Eq => BinaryOperator::StrictEquality,
        BinOp::Ne => BinaryOperator::StrictInequality,
        BinOp::Lt => BinaryOperator::LessThan,
        BinOp::Le => BinaryOperator::LessEqualThan,
        BinOp::Gt => BinaryOperator::GreaterThan,
        BinOp::Ge => BinaryOperator::GreaterEqualThan,
        BinOp::BitAnd => BinaryOperator::BitwiseAnd,
        BinOp::BitOr => BinaryOperator::BitwiseOR,
        BinOp::BitXor => BinaryOperator::BitwiseXOR,
        BinOp::Shl => BinaryOperator::ShiftLeft,
        BinOp::Shr => BinaryOperator::ShiftRight,
        BinOp::And | BinOp::Or => unreachable!("handled above"),
    };
    self.ast.expression_binary(SPAN, left, operator, right)
}
```

Also update `emit_func` to emit declared parameters: build a `FormalParameter` per name (a binding-identifier pattern) and push into the `formal_parameters` vec. Model on `reference/.../emit.rs` (search `formal_parameter`), reconciling with oxc 0.138.

> Verify the oxc 0.138 enum variant names (`BinaryOperator::*`, `LogicalOperator::*`, `UnaryOperator::*`, `Argument::from`, `expression_logical`, `expression_unary`, `expression_call`) — names occasionally change between oxc versions.

- [ ] **Step 4: Run the emit tests**

Run: `cargo +nightly test -p nymph-codegen`
Expected: PASS — arithmetic, params, calls, and string/bool/char literals emit.

- [ ] **Step 5: Format, clippy, commit**

```bash
cargo +nightly fmt && cargo +nightly clippy -p nymph-codegen --all-targets
git add crates/nymph-codegen
git commit -m "feat(codegen): emit scalar exprs, operators, calls, params"
```

---

## Task 4: Structural AST→HIR lowering for functions and scalar expressions

**Files:**
- Create: `crates/nymph-sema/src/lower_hir.rs`
- Modify: `crates/nymph-sema/src/lib.rs`
- Test: `crates/nymph-sema/tests/lower_hir.rs`

**Interfaces:**
- Consumes: `nymph_ast::decl::{Module, Declaration, FuncDeclaration}`, `nymph_ast::expr::{Expr, ExprKind, Statement}`, `nymph_ast::ops::{BinaryOperator, PrefixOperator}`, `nymph_hir::hir::*`.
- Produces: `nymph_sema::lower_hir(module: &Module) -> HirModule`.

- [ ] **Step 1: Write failing lowering tests**

Create `crates/nymph-sema/tests/lower_hir.rs`:

```rust
use nymph_hir::hir::{BinOp, HirExpr, HirModule};
use nymph_sema::lower_hir;
use nymph_syntax::parse_module;

fn lower(src: &str) -> HirModule {
    let parsed = parse_module(src, "test");
    assert!(
        !parsed.diagnostics.iter().any(|d| d.is_error()),
        "parse failed: {src}"
    );
    lower_hir(&parsed.tree)
}

#[test]
fn lowers_a_function_with_arithmetic() {
    let hir = lower("func f(a: int, b: int): int = a + b");
    assert_eq!(hir.funcs.len(), 1);
    let f = &hir.funcs[0];
    assert_eq!(f.name, "f");
    assert_eq!(f.params, vec!["a".to_string(), "b".to_string()]);
    assert_eq!(
        f.body,
        HirExpr::Binary {
            op: BinOp::Add,
            lhs: Box::new(HirExpr::Local("a".into())),
            rhs: Box::new(HirExpr::Local("b".into())),
        }
    );
}

#[test]
fn lowers_a_call_and_int_literal() {
    let hir = lower("func g(): int = h(1)");
    assert_eq!(
        hir.funcs[0].body,
        HirExpr::Call {
            callee: Box::new(HirExpr::Local("h".into())),
            args: vec![HirExpr::Num(1.0)],
        }
    );
}
```

- [ ] **Step 2: Run; expect failure**

Run: `cargo +nightly test -p nymph-sema --test lower_hir`
Expected: FAIL to compile — `lower_hir` does not exist.

- [ ] **Step 3: Write the lowering pass**

Create `crates/nymph-sema/src/lower_hir.rs`:

```rust
//! Structural lowering of the AST into the code-generation HIR.
//!
//! Slice 1 is a pure syntactic walk: it consumes neither type annotations nor the
//! interner, because JS needs no type information to emit correct scalar/control-flow
//! code (see the slice-1 plan's Design Decisions). Later slices thread annotations
//! through here for value-copy insertion and operator-overload dispatch.

use nymph_ast::{
    decl::{Declaration, FuncDeclaration, Module},
    expr::{Expr, ExprKind, Statement},
    ops::{BinaryOperator, PrefixOperator},
};
use nymph_hir::hir::{BinOp, HirExpr, HirFunc, HirModule, HirStmt, UnOp};

/// Lower a checked module into the code-generation HIR.
pub fn lower_hir(module: &Module) -> HirModule {
    let mut funcs = Vec::new();
    for decl in &module.members {
        if let Declaration::Func { meta, body, .. } = decl {
            funcs.push(lower_func(meta, body));
        }
    }
    HirModule { funcs }
}

fn lower_func(meta: &FuncDeclaration, body: &Expr) -> HirFunc {
    let params = meta
        .params
        .iter()
        .map(|p| param_name(&p.0.name))
        .collect();
    HirFunc {
        name: meta.name.0.clone(),
        params,
        body: lower_expr(body),
    }
}

/// The bound name of a simple parameter pattern. Slice 1 supports plain-identifier
/// parameters; destructuring parameters arrive with pattern lowering (Slice 3).
fn param_name(pattern: &nymph_ast::Spanned<nymph_ast::expr::Pattern>) -> ecow::EcoString {
    match &pattern.0 {
        nymph_ast::expr::Pattern::Binding { name, .. } => name.0.clone(),
        other => panic!("slice-1 lowering supports only identifier params, got {other:?}"),
    }
}

fn lower_expr(expr: &Expr) -> HirExpr {
    match &expr.kind {
        ExprKind::Int(v) => HirExpr::Num(v.0 as f64),
        ExprKind::UInt(v) => HirExpr::Num(v.0 as f64),
        ExprKind::Float(v) => HirExpr::Num(v.0.into_inner()),
        ExprKind::Boolean(b) => HirExpr::Bool(b.0),
        ExprKind::Char(c) => HirExpr::Char(c.0),
        ExprKind::Identifier(name) => HirExpr::Local(name.0.clone()),
        ExprKind::Grouped(inner) => lower_expr(inner),
        ExprKind::Call { func, args, .. } => HirExpr::Call {
            callee: Box::new(lower_expr(func)),
            args: args.iter().map(|a| lower_expr(&a.0.value)).collect(),
        },
        ExprKind::BinaryOp { lhs, op, rhs } => HirExpr::Binary {
            op: lower_binop(*op),
            lhs: Box::new(lower_expr(lhs)),
            rhs: Box::new(lower_expr(rhs)),
        },
        ExprKind::PrefixOp { op, value } => HirExpr::Unary {
            op: lower_prefix(*op),
            operand: Box::new(lower_expr(value)),
        },
        ExprKind::Block { body, .. } => lower_block(body),
        ExprKind::If {
            condition,
            then,
            otherwise,
        } => HirExpr::If {
            cond: Box::new(lower_expr(condition)),
            then: Box::new(lower_expr(then)),
            otherwise: otherwise.as_ref().map(|e| Box::new(lower_expr(e))),
        },
        ExprKind::While {
            condition, body, ..
        } => HirExpr::While {
            cond: Box::new(lower_expr(condition)),
            body: Box::new(lower_expr(body)),
        },
        other => panic!("slice-1 lowering does not yet handle {other:?}"),
    }
}

fn lower_block(body: &[nymph_ast::Spanned<Statement>]) -> HirExpr {
    let mut stmts = Vec::new();
    let mut tail = None;
    for (i, stmt) in body.iter().enumerate() {
        let is_last = i + 1 == body.len();
        match &stmt.0 {
            Statement::Let { meta, value } => stmts.push(HirStmt::Let {
                name: param_name(&meta.pattern),
                mutable: meta.mutable,
                value: lower_expr(value),
            }),
            Statement::Expr(e) => {
                if is_last {
                    tail = Some(Box::new(lower_expr(e)));
                } else {
                    stmts.push(HirStmt::Expr(lower_expr(e)));
                }
            }
        }
    }
    HirExpr::Block { stmts, tail }
}

fn lower_binop(op: BinaryOperator) -> BinOp {
    use BinaryOperator as B;
    match op {
        B::Plus => BinOp::Add,
        B::Minus => BinOp::Sub,
        B::Times => BinOp::Mul,
        B::Divide => BinOp::Div,
        B::Remainder => BinOp::Rem,
        B::Power => BinOp::Pow,
        B::Equals => BinOp::Eq,
        B::NotEquals => BinOp::Ne,
        B::LessThan => BinOp::Lt,
        B::LessThanEquals => BinOp::Le,
        B::GreaterThan => BinOp::Gt,
        B::GreaterThanEquals => BinOp::Ge,
        B::BoolAnd => BinOp::And,
        B::BoolOr => BinOp::Or,
        B::BitAnd => BinOp::BitAnd,
        B::BitOr => BinOp::BitOr,
        B::BitXor => BinOp::BitXor,
        B::LeftShift => BinOp::Shl,
        B::RightShift => BinOp::Shr,
        other => panic!("slice-1 lowering does not yet handle operator {other:?}"),
    }
}

fn lower_prefix(op: PrefixOperator) -> UnOp {
    match op {
        PrefixOperator::Negate => UnOp::Neg,
        PrefixOperator::Not => UnOp::Not,
        other => panic!("slice-1 lowering does not yet handle prefix {other:?}"),
    }
}
```

> Verify against the real AST: the exact field names on `Declaration::Func` (`meta`/`body`), `FuncDeclaration` (`name`, `params`, each `FuncParam` with `.0.name`), `LetDeclaration` (`pattern`, `mutable`), the `BinaryOperator`/`PrefixOperator` variant names, and that `Statement::Let` carries `meta`/`value`. Fix any mismatch the compiler reports (these are in `crates/nymph-ast/src/{decl,expr,ops}.rs`). `B::Pipe` and any operator not listed should keep the `panic!` arm — Slice 1 test programs avoid them.

- [ ] **Step 4: Export and build**

In `crates/nymph-sema/src/lib.rs`, add `mod lower_hir;` and `pub use lower_hir::lower_hir;`. Run: `cargo +nightly test -p nymph-sema --test lower_hir`
Expected: PASS.

- [ ] **Step 5: Format, clippy, commit**

```bash
cargo +nightly fmt && cargo +nightly clippy -p nymph-sema --all-targets
git add crates/nymph-sema
git commit -m "feat(sema): structural AST->HIR lowering for scalar/control-flow core"
```

---

## Task 5: Emit blocks, `let`/`mut`, and statement-position `if`/`while`; end-to-end compile + Node run

**Files:**
- Modify: `crates/nymph-codegen/src/emit.rs` (blocks, statements, statement-position control flow)
- Create: `crates/nymph-codegen/tests/run_node.rs` (execution harness)
- Create: `crates/nymph-codegen/src/lib.rs` `compile` entry is NOT here (kept in a driver task); this task wires emit + a test-local compile helper.

**Interfaces:**
- Consumes: `HirExpr::{Block, If, While}`, `HirStmt::{Let, Expr}`.
- Produces: `Emitter` emits block/statement/`if`/`while` in **statement** position (a body that is a block lowers to a JS block with a trailing `return`).

- [ ] **Step 1: Write a Node-execution test (RED)**

Create `crates/nymph-codegen/tests/run_node.rs`:

```rust
//! End-to-end: parse -> check -> lower -> emit -> run under Node, asserting stdout.

use std::io::Write;
use std::process::Command;

use nymph_codegen::emit;
use nymph_sema::{check_module, lower_hir};
use nymph_syntax::parse_module;

/// Compile a Nymph source module to a JS module string.
fn compile(src: &str) -> String {
    let parsed = parse_module(src, "test");
    assert!(
        !parsed.diagnostics.iter().any(|d| d.is_error()),
        "parse errors in test source"
    );
    let checked = check_module(&parsed.tree);
    assert!(checked.diags.is_empty(), "check errors: {:?}", checked.diags);
    emit(&lower_hir(&parsed.tree))
}

/// Emit `src`, append a driver that logs `expr`, run under Node, return trimmed stdout.
fn run(src: &str, call: &str) -> String {
    let mut js = compile(src);
    js.push_str(&format!("\nconsole.log({call});\n"));

    let dir = std::env::temp_dir();
    let path = dir.join(format!("nymph_run_{}.mjs", std::process::id()));
    let mut file = std::fs::File::create(&path).unwrap();
    file.write_all(js.as_bytes()).unwrap();

    let output = Command::new("node").arg(&path).output().expect("run node");
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
    let src = "func compute(): int = {\n  let x = 10\n  let y = x + 5\n  y * 2\n}";
    let out = run(src, "compute()");
    assert_eq!(out, "30");
}
```

- [ ] **Step 2: Run; expect failure**

Run: `cargo +nightly test -p nymph-codegen --test run_node runs_a_block_with_bindings`
Expected: FAIL — block emission hits the `unreachable!` from Task 3 (blocks not yet emitted).

- [ ] **Step 3: Implement block/statement emission**

In `crates/nymph-codegen/src/emit.rs`, add the `JsValue` split (mirroring `reference/.../emit.rs:29-66`) so a value-position control-flow expression yields *leading statements* + a *final expression*, wrapped in an IIFE only when there are statements. For this task implement:
- `emit_stmt(&HirStmt) -> Statement` — `Let` → a JS `const`/`let` variable declaration (`mutable` selects `VariableDeclarationKind::Let` vs `Const`); `Expr` → `statement_expression`.
- `emit_value(&HirExpr) -> JsValue` — for `Block { stmts, tail }`: emit each stmt, then the tail becomes the final expression (or `undefined` if absent). For a scalar/`Binary`/`Call` expr, `JsValue { stmts: empty, expr: emit_expr(...) }`.
- Replace the `unreachable!` for `Block`/`If`/`While` in `emit_expr` with `self.emit_value(expr).into_expression(self.ast)` so an expression-context control-flow node collapses to an expression (IIFE when needed).
- In `emit_func`, if the body is a `Block`, emit its statements directly into the function body followed by `return <tail>;` (avoids a needless IIFE for the common function-body case).

Provide the full `JsValue` struct and `emit_value`/`emit_stmt` implementations, adapting the reference's oxc 0.123 calls to 0.138 (verify `variable_declaration`, `variable_declarator`, `statement_expression`, `expression_arrow_function`, `expression_call`, `binding_pattern`, `binding_identifier`).

- [ ] **Step 4: Run the execution tests**

Run: `cargo +nightly test -p nymph-codegen --test run_node`
Expected: PASS — `runs_arithmetic` → `11`, `runs_a_block_with_bindings` → `30`.

- [ ] **Step 5: Format, clippy, commit**

```bash
cargo +nightly fmt && cargo +nightly clippy -p nymph-codegen --all-targets
git add crates/nymph-codegen
git commit -m "feat(codegen): emit blocks, let/mut, and run emitted JS under Node"
```

---

## Task 6: Value-position `if`/`while`, and the acceptance program

**Files:**
- Modify: `crates/nymph-codegen/src/emit.rs` (`if`/`while` in both positions via `JsValue`)
- Test: `crates/nymph-codegen/tests/run_node.rs`

**Interfaces:**
- Consumes: `HirExpr::{If, While}` through `emit_value`.
- Produces: `emit_value` handles `If` (as a value: hoist a temp `let __r; if (c) { __r = then } else { __r = else }`, final expr `__r`) and `While` (a statement; its value is `undefined`).

- [ ] **Step 1: Write the failing acceptance test**

Add to `crates/nymph-codegen/tests/run_node.rs`:

```rust
#[test]
fn runs_if_as_value() {
    let src = "func sign(n: int): int = if n > 0 { 1 } else { if n < 0 { -1 } else { 0 } }";
    assert_eq!(run(src, "sign(5)"), "1");
    assert_eq!(run(src, "sign(-3)"), "-1");
    assert_eq!(run(src, "sign(0)"), "0");
}

#[test]
fn runs_while_loop() {
    // Sum 1..=n with a mutable accumulator and a while loop.
    let src = "func sum_to(n: int): int = {\n  mut total = 0\n  mut i = 1\n  while i <= n {\n    total = total + i\n    i = i + 1\n  }\n  total\n}";
    assert_eq!(run(src, "sum_to(5)"), "15");
}
```

> Note: `total = total + i` is an assignment expression. If Slice-1 lowering does not yet handle `ExprKind::AssignOp`, add it: lower `a = b` to a JS assignment. Extend `HirExpr` with `Assign { target: Box<HirExpr>, value: Box<HirExpr> }` and `lower_expr`'s `ExprKind::AssignOp` arm (plain `=` only for slice 1; compound `+=` desugars to `target = target + value`), and emit it via oxc `expression_assignment`. Include this in Step 3.

- [ ] **Step 2: Run; expect failure**

Run: `cargo +nightly test -p nymph-codegen --test run_node runs_if_as_value`
Expected: FAIL — `if`-as-value not yet emitting a usable value (or assignment unhandled).

- [ ] **Step 3: Implement value-position `if`/`while` and assignment**

In `emit.rs`, implement `emit_value` for `If`: allocate a gensym temp, emit `let <tmp>;` then an `if` statement whose branches assign `<tmp>`, and return `JsValue { stmts: [decl, if], expr: <tmp ident> }`. For `While`: emit the loop as a leading statement and return `JsValue { stmts: [while], expr: undefined }`. Add `HirExpr::Assign` (Task-1 addition, mirrored in lowering) and emit it with `expression_assignment` (`AssignmentOperator::Assign`, an assignment target from the identifier). Add the gensym counter to `Emitter` (mirror `reference/.../emit.rs:91`). Adapt all oxc calls to 0.138.

- [ ] **Step 4: Run the acceptance tests**

Run: `cargo +nightly test -p nymph-codegen`
Expected: PASS — `sign(±/0)` → `1/-1/0`, `sum_to(5)` → `15`, plus all earlier emit/run tests.

- [ ] **Step 5: Full workspace gate**

Run: `cargo +nightly test && cargo +nightly clippy --all-targets && cargo +nightly fmt --check`
Expected: all PASS/clean — Slice 0 tests plus the new codegen/lowering/run tests.

- [ ] **Step 6: Format, commit**

```bash
git add crates/nymph-codegen crates/nymph-hir crates/nymph-sema
git commit -m "feat(codegen): value-position if/while + assignment; arithmetic/control-flow programs run under Node"
```

---

## Task 7: Public `compile` entry and acceptance lock

**Files:**
- Modify: `crates/nymph-codegen/src/lib.rs` (add a `compile` convenience that ties the pipeline together, or document that the driver crate will own it)
- Test: `crates/nymph-codegen/tests/run_node.rs`

**Interfaces:**
- Produces: `nymph_codegen::compile(source: &str, path: &str) -> Result<String, Vec<Diagnostic>>` — parse → check → lower → emit, returning JS or the checker's diagnostics.

- [ ] **Step 1: Write the failing test**

Add to `crates/nymph-codegen/tests/run_node.rs`:

```rust
#[test]
fn compile_reports_check_errors() {
    // A type error should surface as diagnostics, not JS.
    let result = nymph_codegen::compile("func f(): int = true", "test");
    assert!(result.is_err(), "type error should not produce JS");
}

#[test]
fn compile_produces_runnable_js() {
    let result = nymph_codegen::compile("func double(n: int): int = n * 2", "test");
    assert!(result.is_ok());
}
```

- [ ] **Step 2: Run; expect failure**

Run: `cargo +nightly test -p nymph-codegen compile_produces_runnable_js`
Expected: FAIL — `compile` does not exist.

- [ ] **Step 3: Implement `compile`**

In `crates/nymph-codegen/src/lib.rs`, add (adding `nymph-syntax`, `nymph-sema`, `nymph-diagnostics` as `nymph-codegen` dependencies in its `Cargo.toml`):

```rust
use nymph_diagnostics::Diagnostic;

/// Compile Nymph source to a JS module string, or return checker diagnostics.
/// Lowering runs only on error-free programs.
pub fn compile(source: &str, path: &str) -> Result<String, Vec<Diagnostic>> {
    let parsed = nymph_syntax::parse_module(source, path);
    let mut diags: Vec<Diagnostic> = parsed
        .diagnostics
        .iter()
        .filter(|d| d.is_error())
        .cloned()
        .collect();
    let checked = nymph_sema::check_module(&parsed.tree);
    diags.extend(checked.diags.iter().filter(|d| d.is_error()).cloned());
    if !diags.is_empty() {
        return Err(diags);
    }
    Ok(emit(&nymph_sema::lower_hir(&parsed.tree)))
}
```

> Confirm `parse_module`'s return type exposes `.diagnostics` and `.tree`, and that `Diagnostic` is `Clone` + has `is_error()`. Adjust the diagnostic plumbing to the real API.

- [ ] **Step 4: Run and gate**

Run: `cargo +nightly test -p nymph-codegen`
Expected: PASS — both `compile` tests plus all earlier tests.

- [ ] **Step 5: Format, clippy, commit**

```bash
cargo +nightly fmt && cargo +nightly clippy --all-targets
git add crates/nymph-codegen
git commit -m "feat(codegen): public compile(source) entry (parse->check->lower->emit)"
```

---

## Self-Review

**Spec coverage (against the codegen design's "Slice 1 — Core expressions & functions"):**
- Literals (with widening) → `HirExpr::Num`/`Str`/`Bool`/`Char`; widening is codegen-invisible in JS (Design Decision 1) ✓
- Identifiers → `HirExpr::Local` ✓
- `let`/`mut` → `HirStmt::Let` + JS `const`/`let` (Task 5) ✓
- Calls → `HirExpr::Call` (Tasks 3–4) ✓
- Blocks → `HirExpr::Block` (Tasks 4–5) ✓
- `if`/`while` statement + value position → Tasks 5–6 ✓
- Free functions → `HirFunc` (Tasks 2–4) ✓
- Milestone "arithmetic/control-flow programs run and print correct results under Node" → Tasks 5–6 execution tests (`add`, `compute`, `sign`, `sum_to`) ✓
- Primitive operators → native JS via `BinOp` (Design Decision 2) ✓

**Deferred (correctly, to later slices, per the design):** operator-overload dispatch / `Resolution` consumption (Slice 4), value-copy for tuples + interner threading (Slice 2), pattern matching (Slice 3), ranges (Slice 4), structs/enums/collections (Slice 2). Assignment (`=`) is pulled into Slice 1 (Task 6) because `while`-loop test programs need it.

**Placeholder scan:** No "TBD"/"handle edge cases". The oxc emit code is given at reference-0.123 shapes with an explicit, repeated instruction to reconcile with 0.138 (unavoidable — the exact 0.138 signatures can only be pinned by compiling; the Task 2 spike de-risks this first). AST field/variant names in lowering are given with a "verify against `crates/nymph-ast/src/*`" instruction because they are read, not guessed.

**Type consistency:** `HirModule.funcs`, `HirFunc { name, params, body }`, `HirExpr::{Num, Str, Bool, Char, Local, Call, Binary, Unary, Block, If, While, Assign}`, `HirStmt::{Let, Expr}`, `BinOp`/`UnOp` variant names — consistent across Tasks 1, 3, 4, 5, 6. `emit`/`compile`/`lower_hir` signatures match across codegen and sema tasks. `HirExpr::Assign` is introduced in Task 6 and must also be added to Task 1's enum when implemented (noted in Task 6 Step 3).

**Scope:** One coherent slice producing runnable JS for the scalar/control-flow core, gated by execution under Node. Larger data-type/pattern/operator features are separate later slices.
