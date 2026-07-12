# Slice 4B: Operator Overloading (User-Type Dispatch) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `a + b` on a user struct with a `Plus` impl compiles to `a.plus(b)` and runs under Node; primitive arithmetic keeps emitting native JS operators; every case we can't yet compile correctly panics loudly in lowering instead of silently miscompiling.

**Architecture:** The checker already resolves operators through stdlib interfaces (`solve.rs` pins this). What's missing is the recording + consumption path: (1) `infer_binary` records a per-node `Resolution` into `Annotations` (the `record_resolution` plumbing exists unused since Slice 2A), (2) lowering collects interface-impl methods into `HirClass.methods` so the emitted JS class actually has `plus`, (3) lowering's `BinaryOp` arm branches on the recorded dispatch: builtin → `HirExpr::Binary`, user impl → `Call{Field{lhs, method}, [rhs]}`. Codegen is untouched — `Call{Field{..}}` already emits `recv.name(args)`.

**Tech Stack:** Rust nightly (ALWAYS `cargo +nightly` — the shell exports a stale stable `RUSTUP_TOOLCHAIN`), cargo-nextest, node on PATH for codegen tests. Hard tabs (rustfmt.toml). Do NOT use `cat` in shell (aliased to bat); use Read/Edit/Write tools.

## Global Constraints

- Codegen stays **type-free**: emit.rs never consults types. All dispatch decisions are baked into HIR shapes by lowering, driven by checker annotations.
- Deferred features **panic loudly in lowering** (never silent miscompiles). Panic messages start `"slice-4b lowering does not yet …"`.
- No behavior change to type checking: the set of diagnostics for any program must be identical before and after. `solve.rs` (Milestone B tests) is the regression harness.
- Subagents do NOT commit; the controller commits after verifying each task.
- Finish each task with `cargo +nightly fmt` and a clean `cargo +nightly clippy --all-targets --all-features` (no new warnings).

## Design decisions (locked)

**D1 — `Resolution` carries a method name, not a `DefId`.** Impl methods are not DefId'd (`ImplDef.methods` is a name-keyed map in `iface.rs`), and codegen only needs the JS method name. Reshape the (currently unused) `annotate.rs` struct to:

```rust
/// How a binary operator at a specific node must be compiled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
	/// Interface method name the operator resolved to (e.g. `plus`).
	pub method: EcoString,
	pub dispatch: DispatchKind,
}
```

**D2 — `DispatchKind` gains a deferral variant.** Comparison operators on user types resolve through `Comparable`'s *default* methods (`less_than` etc. are interface-provided defaults over `compare_to`). Codegen does not materialize interface default methods, so emitting `a.less_than(b)` would crash at runtime. Record it distinctly; lowering panics on it.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchKind {
	/// Native JS operator, eagerly evaluated (`+`, `-`, `===`, …).
	BuiltinEager,
	/// Native JS short-circuit operator (`&&`, `||`).
	BuiltinShortCircuit,
	/// Method defined directly in a user impl: compile to `lhs.method(rhs)`.
	UserImpl,
	/// Resolved to an interface *default* method body (e.g. `Comparable`'s
	/// `less_than`). Codegen cannot dispatch to these yet — lowering panics.
	UserImplDefaultMethod,
}
```

**D3 — recording rules in `infer_binary`** (every exit path of the binary-op type rules records exactly one `Resolution` on the `BinaryOp` node's id):

| Path | Recorded |
|---|---|
| same-primitive fast path (`int + int`, …) | `BuiltinEager`, method = the operator's interface method name (informational) |
| `==` / `!=` (blanket `Equals`) | `BuiltinEager` (`equals` dispatch deferred to the stdlib slice; JS `===` stands in) |
| builtin `&&` / `||` | `BuiltinShortCircuit` |
| `dispatch_operator` success, impl self-type is a **primitive** (mixed-primitive arithmetic through stdlib impls, e.g. `int + float`) | `BuiltinEager` — stdlib isn't linked until Slice 5; JS numeric operators have the right semantics here |
| `dispatch_operator` success, impl self-type is a **user ADT**, method **directly defined** in the impl | `UserImpl` |
| `dispatch_operator` success, method provided by an interface **default** | `UserImplDefaultMethod` |
| `??`, `in`/`!in`, `\|>` | do NOT need recording — `lower_binop` already panics on `Unwrap`/`In`/`NotIn`/`Pipe` before any dispatch question arises (recording is harmless if it falls out naturally, but not required) |

How to tell "primitive impl self-type" and "directly defined vs default": inspect what `resolve_method` (`solve.rs:264`) returns / can be made to return — it looks methods up in `ImplDef.methods` and falls back to interface defaults, so it knows the distinction at resolution time. Thread a small enum or bool out of it rather than re-deriving. Task 1's implementer verifies the exact seam and reports if the shape differs.

**D4 — lowering `BinaryOp` becomes annotation-driven and loud:**

- `Some(Resolution { dispatch: BuiltinEager | BuiltinShortCircuit, .. })` → existing `HirExpr::Binary` path.
- `Some(Resolution { dispatch: UserImpl, method })` → `HirExpr::Call { callee: Field { recv: lhs, name: method }, args: vec![rhs] }`.
- `Some(Resolution { dispatch: UserImplDefaultMethod, method })` → `panic!("slice-4b lowering does not yet dispatch operator to interface default method {method}")`.
- `None` → `panic!("slice-4b lowering: no operator resolution recorded for binary op {op:?}")` — a missing recording is a checker bug we want to see immediately.

**D5 — impl methods land on the class.** Lowering collects, in addition to inherent methods (4A):
- nested `StructInnerMember::Impl { members, .. }` blocks inside struct bodies (replaces the 4A loud panic for that variant),
- top-level `Declaration::ImplFor { type_: Reference(name), .. }` blocks targeting a struct,
and pushes their `ImplMember::Func`s through the existing `lower_method` into `HirClass.methods`. Non-`Func` members inside them still panic. `ImplFor` targeting a non-struct type panics (reuse/extend the leftover-`methods_by_type` assert pattern). `StructInnerMember::Namespace` keeps its 4A panic. Generic impls (`impl<T> …`) lower the same as non-generic — JS methods are type-erased; do not special-case.

**Out of scope (defer, existing/new loud panics):** unary operator overloading (`Negate`/`Not`/`BitNot` — currently lower to native JS unconditionally; NOT fixed in this slice, noted in ledger as a known silent gap for 4C), `??`/`in`/`!in`/`|>` lowering, `equals` structural dispatch, `Comparable` default-method dispatch (D2 makes it loud), enum operator impls, interface-typed dynamic dispatch, stdlib linkage.

---

### Task 1: Checker records operator `Resolution`

**Files:**
- Modify: `crates/nymph-sema/src/annotate.rs` (reshape `Resolution`/`DispatchKind` per D1/D2; un-dead-code `record_resolution`)
- Modify: `crates/nymph-sema/src/infer_expr.rs` (`infer_binary` ~880-1013, `dispatch_operator`, the `ExprKind::BinaryOp` TODO at ~264)
- Possibly modify: `crates/nymph-sema/src/solve.rs` (`resolve_method` ~264 — expose direct-vs-default + impl self-type primitiveness)
- Test: `crates/nymph-sema/tests/` (new file `operator_resolutions.rs` or extend an existing suite)

**Interfaces:**
- Consumes: existing `Annotations::record_resolution(id, Resolution)`, `Checker::record`, `binary_method`/`comparison_method` name mappers, `resolve_method`.
- Produces: after `check_module`, `checked.annotations` exposes a `Resolution` for every `BinaryOp` node id, per the D3 table. Task 2/3 rely on `resolution(id) -> Option<&Resolution>` -style access (add a small public accessor mirroring how `variants`/`infos` are read by `lower_hir.rs` today — match the existing access pattern, do not invent a new style).

**Steps:**

- [ ] **Step 1: Write failing tests.** A test helper that checks a source and pulls the `Resolution` off the (single) binary op in a named function's body. Cases: `int + int` → `BuiltinEager`; `a && b` on booleans → `BuiltinShortCircuit`; `int + float` (mixed primitive) → `BuiltinEager`; `Vec2 + Vec2` with `impl Plus<Other = Vec2, Output = Vec2> for Vec2 { func plus(other: Vec2): Vec2 = … }` → `UserImpl` with method `"plus"`; `v1 < v2` with an `impl Comparable<Other = Vec2> for Vec2 { func compare_to(other: Vec2): int = 0 }` → `UserImplDefaultMethod` with method `"less_than"`; `p == q` on a user struct → `BuiltinEager`. Finding the node id: walk the checked function's body AST for the `BinaryOp` node (the existing tests show how modules/exprs are traversed; keep the helper local to the test file).
- [ ] **Step 2: Run them, verify they fail** (`cargo +nightly test -p nymph-sema --test <file>`; expect: resolution absent / types don't exist yet).
- [ ] **Step 3: Implement** per D1–D3. Keep `record_resolution`'s precondition (node already recorded) — record from the `ExprKind::BinaryOp` arm in `infer` after `infer_binary` returns, or restructure minimally; verify against the wrapper's actual control flow.
- [ ] **Step 4: Run the new tests + the full sema suite** (`cargo +nightly test -p nymph-sema`) — new tests pass, zero diagnostics changes (solve.rs untouched and green).
- [ ] **Step 5: fmt + clippy clean. Report; controller commits.**

### Task 2: Lowering collects interface-impl methods onto classes

**Files:**
- Modify: `crates/nymph-sema/src/lower_hir.rs` (`lower_module` — the struct-inner member loop and the top-level collection pass)
- Test: `crates/nymph-sema/tests/lower_hir.rs`

**Interfaces:**
- Consumes: existing `lower_method(meta, body)`, `HirClass.methods`, the 4A panic sites.
- Produces: `HirClass.methods` contains, in source order, inherent methods + nested-impl methods + top-level-`ImplFor` methods for that struct. Task 3's `a.plus(b)` dispatch depends on this.

**Steps:**

- [ ] **Step 1: Write failing tests** in `tests/lower_hir.rs`: (a) struct with nested `impl Plus<Other = V, Output = V> { func plus(other: V): V = … }` → class has method `"plus"`; (b) top-level `impl Plus<…> for V { … }` → same; (c) `#[should_panic(expected = "non-struct")]`-style test for `ImplFor` targeting an enum. (Verify exact Nymph syntax against `stdlib/src/math/complex.nym` and the solve.rs test sources — copy a known-parsing shape.)
- [ ] **Step 2: Verify (a)/(b) fail** — (a) currently panics via the 4A guard, (b) currently yields a class without the method.
- [ ] **Step 3: Implement** per D5.
- [ ] **Step 4: Full sema suite green; fmt + clippy clean. Report; controller commits.**

### Task 3: Lowering dispatches `BinaryOp` on the recorded `Resolution`

**Files:**
- Modify: `crates/nymph-sema/src/lower_hir.rs` (`ExprKind::BinaryOp` arm ~251; `lower_binop` stays for the builtin path)
- Test: `crates/nymph-sema/tests/lower_hir.rs`

**Interfaces:**
- Consumes: Task 1's resolution accessor; Task 2's class methods (for end-to-end coherence, not a code dependency).
- Produces: HIR where user-impl operators are `Call{Field{..}}` — no new HIR nodes, no codegen change.

**Steps:**

- [ ] **Step 1: Write failing tests:** `Vec2 + Vec2` (with impl) lowers to `Call { callee: Field { recv: Local("a"), name: "plus" }, args: [Local("b")] }`; `int + int` still lowers to `Binary { op: Add, .. }`; `#[should_panic(expected = "default method")]` for `v1 < v2` via `Comparable`.
- [ ] **Step 2: Verify the first fails** (currently lowers to `Binary`).
- [ ] **Step 3: Implement** per D4.
- [ ] **Step 4: Full sema suite green (existing `lowers_a_function_with_arithmetic` must still pass — it exercises the `None`-must-not-happen path being newly loud, so if it panics, Task 1's recording has a gap: fix THERE, not by softening the panic). fmt + clippy. Report; controller commits.**

### Task 4: End-to-end Node tests

**Files:**
- Test: `crates/nymph-codegen/tests/run_node.rs` (read the file first; the `call` driver string is raw **JS**, so construct with `new Vec2({ x: 1, y: 2 })`, not Nymph syntax)

**Interfaces:**
- Consumes: everything above; zero production-code changes expected in this task. If emit.rs turns out to need a change, STOP and report — that contradicts the survey and the controller must re-check.

**Steps:**

- [ ] **Step 1: Add tests:** (a) `Vec2 + Vec2` via nested impl → assert component math (e.g. `(new Vec2({x:1,y:2})).plus(new Vec2({x:3,y:4})).x` → `4`, and using the Nymph source's `v1 + v2` inside a Nymph function, e.g. `func add(a: Vec2, b: Vec2): Vec2 = a + b`); (b) same via top-level `impl … for`; (c) an operator used inside a method body (`this.x + other.x` on floats stays native while the outer `+` dispatches); (d) mixed `int + float` function still returns the right number (native `+`).
- [ ] **Step 2: `cargo +nightly test -p nymph-codegen --test run_node`** — all green (30 existing + new).
- [ ] **Step 3: Full workspace `cargo +nightly nextest run`; fmt + clippy. Report; controller commits.**

### Task 5 (controller): ledger + review

- [ ] Update `.superpowers/sdd/progress.md` with a Slice 4B section (scope, per-task commits, deferrals: unary overloads noted as a KNOWN SILENT GAP for 4C, `??`/`in`/`|>`, equals dispatch, Comparable defaults, enum operator impls).
- [ ] Dispatch the standard independent review subagent; address findings; record outcome.
