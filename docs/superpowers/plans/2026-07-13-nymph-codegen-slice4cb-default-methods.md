# Slice 4C-b (Interface Default Method Materialization) Implementation Plan

> **For agentic workers:** Executed by a single dynamic Workflow (investigate →
> implement TDD → review → adversarial refute → fix loop), per the standing
> per-feature-workflow directive. The controller commits and updates the ledger.

**Goal:** Interface default methods (a body defined in the `interface` block,
not overridden by the impl) work on concrete struct types — both as explicit
calls (`v.less_than(w)`) and behind operators (`v1 < v2` via a Comparable-style
interface). This closes the `UserImplDefaultMethod` lowering panic for concrete
ADTs; it remains (correctly) a panic for generic-bound receivers.

**Architecture:** Lowering materializes un-overridden interface default methods
onto each implementing struct's `HirClass` by lowering the interface AST
declaration's default `Func` bodies — the same `lower_method` path impl methods
use. The checker must have type-checked and annotated those default bodies
(verify whether it already does; if not, add it). The dispatch mapping in
`infer_expr.rs:1325` splits: `MethodSource::InterfaceDefault` →
`DispatchKind::UserImpl` (the method now exists on the class);
`MethodSource::GenericBound` → stays `UserImplDefaultMethod` (still a loud
lowering deferral — no dynamic dispatch).

## Global Constraints

- Codegen stays type-free; dispatch decisions are baked into HIR by lowering.
- Deferred features panic loudly in lowering (never silent miscompiles).
- Rust: `cargo +nightly`, hard tabs, 2-space width, clippy clean.
- Subagents do not commit; the controller commits with jj scoped commits.

## Current state (surveyed 2026-07-13, post-4c83caac)

- `InterfaceDef { generics, methods: FxHashMap<EcoString, IfaceMethod> }` and
  `ImplDef` (crates/nymph-sema/src/iface.rs:33-69) store SIGNATURES only —
  default bodies exist only in the AST (`Declaration::Interface` members).
- `resolve_method` → `method_signature` returns `MethodSource::InterfaceDefault`
  when the impl doesn't override the interface's method (solve.rs:495-534).
- `infer_expr.rs:1325`: `InterfaceDefault | GenericBound` both map to
  `DispatchKind::UserImplDefaultMethod` → lowering panic. After this slice the
  arms split (InterfaceDefault → UserImpl; GenericBound unchanged).
- Lowering already collects impl methods onto struct classes from three sites
  (top-level `Declaration::Impl`/`ImplFor`, struct-inner impls; Slice 4B
  commit 151f3cae) and panics on non-struct targets via the leftover
  `methods_by_type` assert.
- stdlib is NOT linked into checked modules yet — tests declare local
  interfaces (mirroring `comparable_less_than_is_interface_default_method` in
  crates/nymph-sema/tests/operator_resolutions.rs).

## Decisions

- **V1 (materialization site):** lowering, not the checker: for each struct
  impl (`ImplFor` top-level or struct-inner interface impl), after pushing the
  impl's own methods, push lowered copies of the interface's default-bodied
  methods that the impl did NOT override. Interface resolution is by name from
  the same module's AST (stdlib unlinked). Deterministic order: impl-provided
  methods first (source order), then un-overridden defaults (interface source
  order).
- **V2 (dispatch split):** `MethodSource::InterfaceDefault` →
  `DispatchKind::UserImpl`. `GenericBound` keeps `UserImplDefaultMethod`; the
  lowering panic message stays accurate for it. Update stale comments/names if
  they now mislead.
- **V3 (checker prerequisite):** default bodies must be checked + annotated
  (self bound to the interface's SelfTy, method params in scope) so
  `lower_method` finds annotations. INVESTIGATE whether members.rs already
  checks them; if not, add checking mirroring how impl-method bodies are
  checked. Operators inside default bodies resolve generically once — e.g.
  `this.compare_to(other) < 0` is int<int → BuiltinEager, valid for every
  impl. A default body whose operator depends on Self resolves through the
  generic-bound path → UserImplDefaultMethod → loud lowering panic
  (acceptable, documented deferral).
- **V4 (collisions):** two interfaces implemented by the same struct both
  defaulting the same method name → lowering panics loudly (message naming the
  struct + method). Override always wins over default; an impl overriding one
  of two same-named defaults still panics (ambiguity is real). The checker may
  already diagnose some of this — investigate; loud is the floor.
- **V5 (out of scope, loud):** blanket impls (`impl<T> Iface for T`) do NOT
  materialize (verify today's behavior is loud through the collection loop or
  unreachable; if a blanket default is silently dropped, make it panic);
  generic-bound dispatch; enums (existing non-struct panics); stdlib linkage;
  dynamic dispatch through interface-typed values.

## Tasks

### Task 1: Checker — default bodies annotated + dispatch split
Files: crates/nymph-sema/src/members.rs (default-body checking, if missing),
crates/nymph-sema/src/infer_expr.rs (:1325 split), tests:
crates/nymph-sema/tests/operator_resolutions.rs (the existing
`comparable_less_than_is_interface_default_method` test now expects
UserImpl — update it and its name), tests/solve.rs if default-body checking
adds diagnostics paths.

### Task 2: Lowering — materialize defaults onto struct classes
Files: crates/nymph-sema/src/lower_hir.rs, tests:
crates/nymph-sema/tests/lower_hir.rs.
Cases: un-overridden default appears as a class method (lowered body, `this`
works); override wins (impl body used, default not duplicated); same-name
default collision panics; the former `user_comparable_default_method_panics_in_lowering`
should_panic test flips to a positive lowering test; bounded-generic operator
still panics (pin it).

### Task 3: Node end-to-end
Files: crates/nymph-codegen/tests/run_node.rs.
Cases: `v1 < v2` via local Comparable-style interface (compare_to in impl,
less_than defaulted) returns the right boolean; explicit `v.less_than(w)` call
works; default method calling another interface method (`this.compare_to`)
works; override-wins case runs the override.

### Task 4 (controller): commit, ledger, record review outcome.
