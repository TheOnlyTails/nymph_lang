# Codegen Slice 0 (Foundation) — progress

Plan: docs/superpowers/plans/2026-07-05-nymph-codegen-slice0-foundation.md
Branch: codegen
Base (before Task 1): e159d099 (jj working-copy baseline)

- Task 1: complete (commits e159d09..42beed2b, review clean; Minor: review-package under-renders small hunks)
- Task 2: complete (commits 42beed2b..fc896533, controller review clean; parse_expression now returns Expr)
- Task 3: complete (commits fc896533..4b720f14, controller-implemented+reviewed inline; FLAG for extra scrutiny in final review since no independent review). 97 sema + 29 syntax tests green, clippy clean.
- Task 4: complete (commits 4b720f14..2bc22908, controller inline). nymph-hir crate + Ty/ids moved; ena Key newtype for orphan rule; 126 tests green, clippy clean.
- Task 5+6: complete (merged, commit 2bc22908..c52864d9, controller inline; FLAG for independent review). annotate.rs + Checked + recording literals/binary ops. 128 tests green, clippy+fmt clean.
- Task 7: complete. Slice 0 acceptance gate passed (build+fmt-check+clippy+128 tests). Memory updated.
- Whole-branch review: DONE (opus, merge-yes, no Critical). Follow-ups applied in 22e2089c (zonk recorded types + TODO marker). Rejected the "uniform literal recording" finding: int-only is intentional (only int literals widen). Interner-in-Checked deferred to Slice 1 (lowering needs it).
- SLICE 0 COMPLETE + REVIEWED.

## Slice 1 (Core Exprs & Functions)
Plan: docs/superpowers/plans/2026-07-10-nymph-codegen-slice1-core-exprs.md
Base (before Task 1): 115006aa
- Task 1: complete (HIR types, controller-verified inline; trivial pure-data diff). Build/clippy/fmt clean.
- Task 2: complete (oxc 0.138 spike, controller-reviewed inline). NOTE: whole oxc AstBuilder API deprecated in 0.138 (module-scoped allow); allocator Box::leak'd. Both documented slice-1 simplifications.
- Task 3: complete (scalar/operator/call emit, controller-verified inline incl. full BinOp mapping). 3/3 codegen tests pass, clippy/fmt clean.
- Task 4: complete (structural lowering, controller-verified inline). Reconciled LetDeclaration.name + PrefixOperator::BoolNot. 101 sema tests pass, clippy/fmt clean.
- Task 5: complete (blocks/let/mut + FIRST NODE EXECUTION, controller-verified inline). add(3,4)=11, compute()=30 run under node. Harness hardened (temp-file race, FORCE_COLOR). clippy/fmt clean.
- Task 6: complete (value-position if/while + assignment, commit 0702496e). Control-flow programs run under Node.
- Task 7: complete (public compile() entry, commit 94536513). parse->check->lower->emit pipeline.
- SLICE 1 COMPLETE.

## Slice 2A (Collections & Interner Threading)
Plan: docs/superpowers/plans/2026-07-11-nymph-codegen-slice2a-collections.md
Base (before Task 1): fc692d54
- Task 1: complete (Checked+interner, controller-verified inline; clean move). sema+codegen tests green.
- Task 2: complete (uniform recording + index fast-path, controller-verified inline). 102 sema tests green.
- Task 3: complete (HIR collection nodes Array/MapLit/Index/MapGet + typed lowering via Lowerer{annotations,interner}; IndexAccess dispatches Map->MapGet else Index. emit.rs has temporary unreachable! arms until Task 4. Controller-implemented inline; FLAG for independent review). lower_hir 3/3 green, sema unit tests green, codegen builds, clippy/fmt clean. String-literal lowering deferred out of this collections slice (map tests use int keys).
- Task 4: complete (emit Array→JS array, MapLit→new Map([[k,v],…]), Index→recv[i], MapGet→recv.get(k); controller-implemented inline). oxc is 0.139 not 0.138: expression_array dropped its trailing Option arg. runs_list_and_index→30, runs_tuple_roundtrip→[1,2], runs_map_get→6 all run under Node. Full workspace tests green, codegen clippy/fmt clean. Map test uses int keys (strings deferred).
- SLICE 2A COMPLETE. PENDING: independent whole-branch review of Tasks 3-4 (implemented inline, no independent review).

## Slice 2B (Structs & Field Access)
Plan: docs/superpowers/plans/2026-07-11-nymph-codegen-slice2b-structs.md
Base (before Task 1): 57e8661b (Slice 2A Task 4)
Design: structs → JS classes; construction detected via Lowerer struct-name pre-pass (no checker change); labeled ctor args only (positional deferred); field defaults deferred; methods deferred to Slice 5.
- Task 1: complete (HIR class + New/Field nodes, controller inline; commit 83155f99). HirModule.classes, HirClass, HirExpr::New/Field.
- Task 2: complete (lower struct decls→HirClass, construction→New via struct-name pre-pass, MemberAccess→Field; commit 427e4a40). Labeled ctor args only. lower_hir 5/5 green.
- Task 3: complete (emit classes as `class N { constructor(fields){ Object.assign(this,fields) } }`, New→`new N({…})`, Field→`recv.name`; controller inline). Chose Object.assign over per-field assignment (simpler, order-free, no field defaults in 2B). runs_struct_construction_and_field→4, runs_struct_field_through_param→30 under Node. Full workspace green, codegen clippy/fmt clean.
- SLICE 2B COMPLETE.
- Review (Slice 2A+2B): subagent review blocked by session limit; completed inline as controller. No Critical/live bugs — codegen verified type-free; Map-vs-Index dispatch sound (zonked types, no alias TyKind, non-Map falls through to recv[i]); pre-pass mirrors checker's construction dispatch. Follow-up applied in b4f474c7 (sharpened pre-pass comment: real soundness argument + module-local-structs assumption for future imports). Known limitations by the established panic pattern: positional ctor args (type-valid, fails loudly), collection spreads, string-keyed maps. SLICE 2A+2B REVIEWED.

## Slice 2C (Enums & the Symbol-tag Value ABI)
Plan: docs/superpowers/plans/2026-07-11-nymph-codegen-slice2c-enums.md
Base (before Task 1): d73eb0e5
Scope (user chose full bare+qualified via checker recording): enums → TAG=Symbol.for("nymph.tag") + per-enum object of variant factories (fields) / frozen singletons (nullary); checker records (enum,variant) resolution per variant node; lowering emits VariantNew/VariantRef; equality.ts ~tag→[TAG]. Matching (Slice 3), Copy (no mutation path), positional/spread args deferred. NOTE: enum variant syntax uses braces `enum E { V(f: T), N }` (per check.rs), not parens.
- Task 1: complete (HIR enum + VariantNew/VariantRef nodes; commit 5c0c50c5).
- Task 2: complete (checker records (enum,variant) resolution in a NodeId side-table via variant_value/infer_variant_ctor + threaded node id; commit fb59c676). records_variant_resolution green.
- Task 3: complete (lower enum decls→HirEnum, variant construction→VariantNew, nullary ref→VariantRef via annotation; commit 682499c1). lower_hir 6/6 green.
- Task 4: complete (emit TAG=Symbol.for + per-enum IIFE of factories/frozen singletons, VariantNew→E.V({…}), VariantRef→E.V; equality.ts ~tag→[TAG]; controller inline). Chose IIFE-per-enum shape (const E = (()=>{const t=Symbol();return{…}})()) reusing JsValue IIFE machinery, avoiding member-target assignments. Node: mk().value→7, none()===Opt.None, cross-variant tags distinct. 14 codegen Node tests green, equality.ts oxlint-clean, full workspace green.
- SLICE 2C COMPLETE. Deferred: matching/is (Slice 3), Copy (no mutation path), positional/spread args.
## Slice 3A (Pattern Matching — scalar & variant core)
Plan: docs/superpowers/plans/2026-07-11-nymph-codegen-slice3a-matching.md
Base (before Task 1): c6a27fa1
Scope: match with literal/binding/placeholder/variant patterns (nested), leveraging the 2C Symbol-tag ABI. Variant-pattern resolution recorded span-keyed (patterns have no NodeId). Deferred to 3B: guards, tuple/list/map/struct patterns, range/string/union, standalone `is`.
- Task 1: complete (HIR match/arm/pattern nodes: HirExpr::Match, HirArm, HirPat, HirLit; commit 35332a98).
- Task 2: complete (checker records variant-pattern resolution span-keyed in Annotations.pattern_variants, at both nullary + struct-path sites; variant_resolution → pub(crate); commit 12daa882).
- Task 3: complete (lower match → HirExpr::Match, lower_pattern for scalar/binding/placeholder/variant; guards+aggregates panic; commit 75486a33).
- Task 4: complete (compile_pat → (test, bindings) with re-emittable Subject; emit match as const _s/let _r/if-chain built back-to-front, last arm testless; variant test = _s?.[TAG]===E.V[TAG]; controller inline). Node: unwrap_or(Some 42)→42, None→0, classify 100/200/300. 16 codegen Node tests green, full workspace green, clippy/fmt clean.
- SLICE 3A COMPLETE. Deferred to 3B: guards, tuple/list/map/struct patterns, range/string/union, standalone `is`.

## Slice 3B (Pattern Matching — full)
Plan: docs/superpowers/plans/2026-07-11-nymph-codegen-slice3b-patterns-full.md
Base (before Task 1): 8d1f6897
Scope (user chose 3B-full): struct/tuple/list(+rest)/map/range/string/union patterns + guards. NO checker change (guards type-check; structural patterns need no resolution; struct-vs-variant Pattern::Struct distinguished by pattern_variant_of). Guards force a match-emission rewrite: if/else-if chain → labeled block + break (a matched-but-guard-failed arm must fall through). Deferred edges (panic loudly): map-rest, non-literal map keys, interpolated/escaped string patterns, binding unions. `is` expression still deferred.
- Task 1: complete (HIR: HirArm.guard, HirLit::Str, HirPat Struct/Tuple/List/Map/Range/Or, HirRange; Subject::Index; commit db61ffbd).
- Task 2: complete (lower guards + all new pattern forms; helpers lower_lit_pattern/lower_string_pattern/lower_range_pattern; struct-vs-variant via pattern_variant_of; commit 75f528d6). Deferred-edge panics: map-rest, non-literal map keys, escaped strings.
- Task 3: complete (match emission rewritten if/else-if → labeled block + break for guard fall-through; match_arm helper; struct/tuple compile_pat with and_test; commit cc5b945a). All 3A tests still pass (behavior-preserving rewrite).
- Task 4: complete (list/map/range/string/union compile_pat; Subject::IndexFromEnd/MapGet/Slice; compile_range; union binds-nothing guard; controller inline). Node: list #[]/spread+head, range 1..10/10..=100, union Red|Green. 23 codegen Node tests green, full workspace green, clippy/fmt clean.
- SLICE 3B COMPLETE. match is fully general. Deferred (loud panics): map-rest, non-literal map keys, escaped/interpolated string patterns, binding unions. Checker requires `_` arm for list matches (doesn't infer empty+spread coverage).
## nymph-compiler facade crate
- Added nymph-compiler crate (commit d0eec62a): facade over parse→check→lower→emit. compile(source,path)→Result<String,Vec<Diagnostic>>; check(source,path)→Vec<Diagnostic> (all diags for tooling). Re-exports Diagnostic/Severity. Built by sonnet subagent, controller-reviewed + gates re-run. 5 integration tests. Workspace member uncommented. 31 test suites green.

## Slice 4A (Inherent Instance Methods) — PLANNED (not yet executed)
Plan: docs/superpowers/plans/2026-07-11-nymph-codegen-slice4a-methods.md
Base (before Task 1): d0eec62a (nymph-compiler)
KEY FINDINGS from exploration: (1) The checker ALREADY checks inherent method bodies with self_ty set (members.rs) — the members.rs test `top_level_inherent_impl` (struct + impl + `this` + `p.get()`) is assert_ok. So method bodies get annotated → lower correctly. NO checker change needed for 4A. (2) Method CALLS `p.m(args)` already lower structurally via the existing Call→Field path → emit `p.m(args)`. (3) `this` (ExprKind::This) is NOT lowered yet (panics) — needs a HirExpr::This node. (4) Enums emit as the Symbol-tag object (not a class), so enum methods need an ABI decision → DEFERRED. (5) Operator overloading + interface dispatch are genuinely Milestone-B-incomplete in the checker → separate later sub-slices.
Scope (4A): inherent instance methods on STRUCTS only (top-level `impl Point{func…}` + struct-inner Member funcs) → JS class methods; `this` → JS this; method calls already work. Deferred: operators (4B), interfaces, enum methods, namespaced/static methods, external companions, mut methods, ranges, ??/?/as/is.
- Task 1: complete (HIR: HirClass.methods, HirMethod, HirExpr::This; commit 3f504c6e).
- Task 2: complete (ExprKind::This→HirExpr::This; lower_module two-pass method collection from top-level impls + struct-inner Member funcs; lower_method; commit 6968e0c1). Non-Reference/interface impls skipped.
- Task 3: complete (emit This→JS this; emit_method builds class methods, pushed into class body after ctor; controller inline). Node: total(Point 3,4).sum()→7, bump(Counter 5).add(10)→15. 27 codegen Node tests green, full workspace green (31 suites), clippy/fmt clean.
- SLICE 4A COMPLETE. Inherent struct methods + this run under Node. Deferred to later Slice 4 sub-slices: operator overloading (4B, needs checker Milestone-B operator/DispatchKind recording), interface impls/dispatch, enum methods (ABI decision), namespaced/static methods, external companions, mut methods, ranges, ??/?/as/is.

## Slice 4B (Operator Overloading Dispatch)
Plan: docs/superpowers/plans/2026-07-13-nymph-codegen-slice4b-operators.md (decisions D1–D5)
Base: 5c0768fa's parent chain from c5f6be84 (plan commit). Executed via sonnet subagents (Tasks 1–4) + dynamic Workflow runs (review, closeout) per the new per-feature-workflow directive.
- Task 2: complete (commit 151f3cae) — lower_module collects Declaration::Impl AND ImplFor methods onto struct classes; struct-inner `impl Iface { … }` members lowered via lower_method; ImplFor-on-enum panics (was a reachable silent drop).
- Task 1: complete (commit 9ef318d3) — checker records Resolution { method: EcoString, dispatch: DispatchKind } per BinaryOp node (D3 table): same/mixed primitives + ==/!= → BuiltinEager, boolean &&/|| → BuiltinShortCircuit, user-ADT impl → UserImpl, interface default (Comparable less_than) → UserImplDefaultMethod; solve.rs threads MethodSource (Inherent/ImplDirect/InterfaceDefault/GenericBound) out of resolve_method. 6 tests in tests/operator_resolutions.rs pin the table.
- Task 3: complete (commit 31291b22) — lower_binary dispatches on the recorded Resolution: builtins → native HirExpr::Binary, UserImpl → Call{Field{lhs, method},[rhs]}, UserImplDefaultMethod → panic, None → panic. Closes the pre-4B silent miscompile (user-type + emitted JS +).
- Task 4: complete (commit 5c0768fa) — Node e2e: nested-impl and top-level impl-for + both dispatch to .plus(); operator inside method body stays native while outer dispatches; int-literal + float stays native. 198 workspace tests.
- NOTE (checker semantics, intentional): mixed-primitive ops that matched a stdlib impl still record BuiltinEager (impl semantics ≡ native JS); ==/!= ALWAYS record BuiltinEager — there is no UserImplDefaultMethod path for equals at all (native === is reference equality on class instances; user equals dispatch deferred).
- SLICE 4B COMPLETE (with closeout fixes below).

## Slice 4B review + closeout
- Review 1 (dynamic Workflow slice4b-review, 4 sonnet dimensions × 3 adversarial refuters; 13/25 agents lost to a session limit, findings below verified end-to-end by surviving refuters):
  - CRITICAL: compound assignment bypassed the whole Resolution mechanism — `v1 += v2` on a Plus-impl struct type-checked clean and emitted native JS `v1 = v1 + v2` (object string-coercion). Silent miscompile, in nobody's deferral list.
  - IMPORTANT: `func f<T>(a: T, b: T): T = a + b` type-checked clean then ICE'd on lowering's None panic (valid program → compiler panic). Same fallback covered unresolved inference vars — investigation proved a stale-None ICE was reachable on valid programs (`let xs = #[] … xs[0] + xs[0]`).
  - Refuter hygiene note: review agents left zzz_* probe files + a scratch run_node test in the tree; cleaned via jj restore. Future workflows instruct refuters to delete probes.
- Closeout (dynamic Workflow slice4b-closeout: investigate → implement → 2-round review/refute → fix; + one follow-up agent; commit ef2feb27):
  - Compound assign now records the Resolution on the AssignOp node and lowering dispatches it (shared lower_operator helper): builtins native, UserImpl → `a = a.plus(b)` (Node e2e test), default-method/None panic; non-identifier compound targets panic explicitly (codegen only supports Local assign targets today).
  - infer_binary's fallback split: generic-param operands route through dispatch_operator (bound → UserImplDefaultMethod loud lowering deferral; no bound → NotImplemented diagnostic); unresolved inference vars go on a pending_operators queue finalized PER FUNCTION BODY (while that body's param_bounds are live — module-end finalization was declaration-order dependent, caught by review round 2 and fixed: test pins both declaration orders identical); still-unbound → new CannotInferOperandType diagnostic. Lowering's None panic retained as pure invariant guard (test pins it by stripping annotations; no valid-program path reaches it).
  - Latent bug fixed en route: infer_inherent_return's discarded trial run now truncates pending_operators like it truncates diags (was leaking trial deferrals into the real body's drain).
  - 211 workspace tests green, fmt/clippy clean. SLICE 4B REVIEWED + FIXED.
- Deferred from 4B (loud unless noted): unary operator overloads — KNOWN SILENT GAP, MUST be first in 4C (user-type unary ops still emit native JS); ??/in/!in/|> dispatch (lowering panics); user equals dispatch for ==/!= (silently native === — accepted, documented above); Comparable/interface default methods (lowering panics); generic-bound operator dispatch (lowering panics via UserImplDefaultMethod); enum operator impls (lowering panics); stdlib linkage.

## Slice 4C-a (Unary Operator Overloading Dispatch)
Plan: docs/superpowers/plans/2026-07-13-nymph-codegen-slice4c-unary-operators.md (commit 5142f7ee, decisions U1–U4)
Executed as ONE dynamic Workflow (slice4c-unary-operators: investigate → implement → 2-round review/refute → fix) per the per-feature-workflow directive. Implementation commit: 4c83caac.
- Checker: infer's PrefixOp interception records Resolution like BinaryOp/AssignOp; infer_prefix returns (Ty, Option<Resolution>, pending slot). PendingOperatorKind reshaped to carry the operator per variant (BinaryOp(op)/AssignOp(op)/PrefixOp(op)); unresolved negate/bit_not operands defer to the per-body queue (was a spurious "not implemented for `_`" mis-diagnosis on valid late-pinned programs); BoolNot keeps default-to-boolean for primitive-or-Infer operands (never queues). Bounded generic -t → UserImplDefaultMethod (loud lowering deferral); unbounded → NotImplemented (pre-existing behavior, now pinned).
- Lowering: lower_prefix_op mirrors lower_operator — BuiltinEager → HirExpr::Unary, UserImpl → Call{Field{operand, method}, []}, UserImplDefaultMethod/ShortCircuit/None → slice-4c panics.
- Investigation correction to the plan: "no codegen changes" was wrong for `~` — HIR had no UnOp::BitNot (zero-diagnostic `~int` panicked in lowering via the slice-1 catch-all). Added UnOp::BitNot + emit BitwiseNot arm. Parse gotcha recorded for test authors: line-leading `-` continues the previous expression as binary minus; bind unary probes via `let y = -xs[0]`.
- Review: round 1 confirmed one finding (BitNot codegen path had zero test coverage anywhere) → fixed in-loop (native `~int` Node e2e + lowering test); round 2 clean, nothing unresolved. 230 workspace tests green, fmt/clippy clean.
- Deferred (unchanged from 4B list): PostfixOp (`?`/`!` error propagation, Milestone B); interface dynamic dispatch; enum unary impls (non-struct collection panics); ??/in/!in/|>; user ==/!= dispatch; stdlib linkage.
- SLICE 4C-a COMPLETE, REVIEWED + FIXED.

## Slice 4C-b (Interface Default Method Materialization)
Plan: docs/superpowers/plans/2026-07-13-nymph-codegen-slice4cb-default-methods.md (commit 30fbdda3, decisions V1–V5)
Executed as ONE dynamic Workflow (slice4cb-default-methods). Implementation commit: 5b3a752d.
- Checker: check_interface_default_bodies (members.rs) checks each default body once, generically — `this` bound to a rigid synthetic Param bounded by the interface (SelfTy has no head, so plain resolve_method can't see it; the bounded-param trick mirrors how generic function bodies check). New Checker::checking_interface_default field: same-interface calls on `this` inside a default body resolve directly against the interface's abstract signature, bypassing impl/blanket search (stdlib's blanket `impl<T> Comparable for T` was intercepting compare_to and mis-pinning Other=Self → 9 false mismatches).
- Dispatch split (V2): Inherent|ImplDirect|InterfaceDefault → UserImpl (defaults are materialized, callable directly); GenericBound alone stays UserImplDefaultMethod (loud lowering deferral).
- Lowering (V1): per impl (top-level ImplFor + struct-inner), impl methods pushed in source order, then un-overridden interface defaults lowered via the same lower_method path (annotations verified impl-independent: resolutions/variants/map-index are all generic-safe). assert_no_duplicate_methods panics on any class method-name collision (two defaults, override vs sibling default, or double override — the last was a pre-existing silent last-wins).
- Adjacent silent miscompiles CLOSED: explicit `v.less_than(w)` on a default-only method type-checked clean and crashed at runtime under Node ("less_than is not a function") — materialization fixes it, e2e test added; `impl Iface for #[int]` (non-Reference target) was silently dropped → now panics; blanket-impl generic name colliding with a real struct name → explicit panic.
- stdlib bug found+fixed: Comparable's min/max defaults mixed Self with unconstrained Other in return types (unsound, unusable pre-materialization) — removed.
- KNOWN SILENT GAP (pre-existing, documented by investigation, NOT fixed here): comparison/equality/logical operators on BOUNDED-GENERIC operands record BuiltinEager (is_adt excludes Param only on the arithmetic path) → `a < b` under `T: Comparable<Other = T>` emits native JS `<` on objects. Applies to bounded-generic function bodies generally, not just default bodies. Candidate for the next slice.
- Review: round 1 single raw finding refuted, zero confirmed, nothing unresolved. 236 workspace tests green (Node e2e: default-via-operator, explicit default call, override-wins), fmt/clippy clean.
- SLICE 4C-b COMPLETE, REVIEWED CLEAN.

## Slice 4C-c (Comparisons on Non-Concrete Operands)
Plan: docs/superpowers/plans/2026-07-13-nymph-codegen-slice4cc-comparison-generics.md (decisions W1–W5). Implementation commit: 0ec3a9e2. ONE dynamic Workflow; review round 1 had ZERO raw findings.
- Comparison arm reached parity with arithmetic: Param → dispatch_operator (bound → UserImplDefaultMethod loud deferral; none → NotImplemented); Infer → per-body pending queue (fallback resolver is now op-class-aware: comparison_method vs binary_method, and comparison nodes keep their boolean type on finalization — naive reuse would have panicked binary_method's unreachable! AND clobbered the node type).
- Closed silent miscompile: late-pinned ADT comparison (xs[0] < xs[0], xs pinned to #[Vec2] later) recorded BuiltinEager → native JS < on objects; now dispatches to the materialized less_than.
- BEHAVIOR CHANGE: never-pinned comparison operands now diagnose CannotInferOperandType (previously compiled "clean" with a wrong eager resolution).
- Pinned decisions: equality stays always-native === for ALL operand kinds (W2, reference semantics); logical ops on generics were already loud unify-with-boolean type errors (W3, zero code change). 250 tests at commit time.

## Slice 4D (Enum Methods — Prototype ABI)
Plan: docs/superpowers/plans/2026-07-13-nymph-codegen-slice4d-enum-methods.md (decisions X1–X5). Implementation commit: 95ff09ac (original f338c3fd, rewritten by rebase + oxc-API port). Developed in a PARALLEL jj workspace (nymph_lang-ws-enums) while 4C-c ran in main — first use of the workspace-per-feature protocol.
- ABI: enums WITH methods emit a proto object in the enum IIFE; every variant is Object.create(proto)-based — nullary singletons stay frozen, field-variant FACTORY functions keep their own [TAG] (pattern matching reads it off the factory), only the returned object carries the prototype. Method-less enums emit byte-identical JS (pinned by test).
- HirEnum.methods; collect_adt_methods unifies struct+enum collection (top-level impl/impl-for + body-inner members + default materialization + duplicate-name panic).
- Investigation findings: enum-BODY members (inherent funcs, nested impls) type-checked then were SILENTLY DROPPED by lowering (worse than the known top-level panic) — closed; `this.field` on enum receivers is rejected by the checker (field access resolves only on structs) — enum methods read payloads via match(this); checker-side enum dispatch needed zero changes (is_adt/dispatch symmetric with structs).
- Two pre-existing should_panic("non-struct types") tests flipped to positive lowering assertions. 246 tests in-workspace pre-integration.

## Golden-program regression corpus
Commit 6b243b84: crates/nymph-compiler/tests/golden_programs.rs — 29 compile-clean multi-feature programs + 12 Node-executed with asserted stdout, through the nymph-compiler facade; pins the whole implemented surface (user request: known-good Nymph must keep compiling). Parse gotcha documented: match-guard expressions ending in an identifier need parens or `x -> y` parses as a closure.
- FOUR should-work-but-doesn't findings checked in as #[ignore]d tests (un-ignoring them later is the fix's acceptance test):
  1. `return` in a valid function ICEs in lowering (slice-2a catch-all; in NO deferral list).
  2. `let` shadowing emits `const x` twice → invalid JS, SyntaxError at load (SILENT MISCOMPILE).
  3. Top-level `let` silently dropped from the emitted module → ReferenceError at runtime (SILENT MISCOMPILE).
  4. `impl Trait` param sugar rejects concrete call-site arguments (synthetic bound param never instantiates; body-side works).
  Findings 1–3 are the natural next slice.

## Parallel-workspace integration note (2026-07-13)
4C-c (main) and 4D (jj workspace) ran as concurrent workflows; meanwhile the USER landed commits on main directly (0cc9d404 clippy-config move, fad21a9b oxc AstBuilder API migration — a 938-line emit.rs refactor). Integration: 4D stack rebased onto main → 7 real jj conflicts in emit.rs (old-API 4D code vs new API) resolved by a port agent, which ALSO caught 3 stale old-API call sites that auto-merged WITHOUT conflict markers (clean merge ≠ correct — grep new insertions for old idioms after any API-migration rebase). Combined suite 301/301 (+4 ignored corpus findings). jj mechanics that worked: workspace-per-feature for simultaneous agents; jj absorb only with --into fencing (unfenced absorb rewrites mutable reviewed commits; new files never absorb).

## Slice 4A review
- Review (Slice 4A): independent subagent review. Verdict "with fixes". Critical: `impl <Enum> { func … }` type-checks but lowering silently dropped it (no HirClass for enums) → JS crashes at runtime (`c.m is not a function`). FIXED in 13224421 (sonnet subagent, controller-directed): lowering now panics on non-Func impl members (top-level AND struct-inner), and a leftover-`methods_by_type` assert catches impls targeting non-struct types (enums etc.) — one assert covers all such cases since struct lowering consumes entries via `.remove`. Added should_panic test pinning the enum-impl case. 179 workspace tests green, clippy/fmt clean. Important test gaps FILLED (sonnet subagent): Node tests runs_struct_method_with_if_control_flow (if/else on this fields → 9), runs_struct_method_calls_sibling_method (this.base() twice → 42), runs_struct_inner_func (func in struct body, confirmed parses via parse_inner_members → 15). All 3 passed immediately → paths were correct, just untested. 30 codegen Node tests green. SLICE 4A REVIEWED + FIXED.

## Slice 3B review
- Review (Slice 3B): independent subagent review (opus). Verdict "with fixes"; NO Critical. Verified sound: guard fall-through, list index/slice math (traced #[a,...mid,b]), ranges, map repr, nested-variant/subexpr preservation, distinct nested-match labels. Fixes in bd526f7e: added Node tests for suffix-after-rest + map patterns (both passed → paths were already correct, just untested); moved binding-union rejection from codegen assert to lowering (pat_binds + descriptive panic, consistent with sibling deferrals) + should_panic test + codegen debug_assert; skipped redundant length>=0 test for rest-only lists. Skipped Minor: union none-collapse test (contrived pattern), gensym _tN reservation (pre-existing). SLICE 3B REVIEWED + FIXED.
- Review (Slice 3A): independent subagent review (opus). Verdict "with fixes"; NO Critical. Core engine verified sound (exhaustiveness backs testless-last-arm; nested/optional-chaining/binding/char all correct). Fixes applied in 0f0e7ef0: should_panic test pinning guard-in-lowering panic; Node tests for nested variant (Wrap(i=A(n))) + match-as-subexpression (IIFE path); stale exhaustiveness doc comment + _s/_r naming drift fixed. Accepted as loud deferrals (not silent miscompiles, consistent with codebase): guards + string/range/tuple/list/map/union patterns panic in lowering. Binding-with-subpattern (name=pat) doesn't parse in match arms → that HirPat::Binding sub branch unreachable from match (harmless). Noted pre-existing (not fixed, affects if/while too): gensym _tN temps not reserved vs user identifiers. SLICE 3A REVIEWED + FIXED.

---
(Older entries below.)

## Slice 2C review
- Review (Slice 2C): independent subagent review (opus). Verdict "with fixes". Critical: field-variant used as first-class value (`let g = Some`) silently miscompiled → FIXED in 124b1ad3 (checker rejects via FieldVariantAsValue; nullary-as-value stays valid). Added tests (reject/accept, qualified-nullary lowering) + deduped tag_obj in emit_enum. Rejected the "shadowing negative test" idea — empirically a variant-named param is a pattern not a binding, so shadowing a variant with a local is unexpressible (invariant vacuously holds). Accepted as known-limitation: positional variant/struct construction panics in lowering (loud ICE, consistent with the codebase's not-yet-implemented deferral pattern). Deferred: equals-via-[TAG] runtime test (needs stdlib linkage, not wired until Slice 5); the tag-identity ABI it relies on IS covered by runs_enum_variant_tag_distinct. SLICE 2C REVIEWED + FIXED.
