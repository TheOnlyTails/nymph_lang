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

## Slice 4A review
- Review (Slice 4A): independent subagent review. Verdict "with fixes". Critical: `impl <Enum> { func … }` type-checks but lowering silently dropped it (no HirClass for enums) → JS crashes at runtime (`c.m is not a function`). FIXED in 13224421 (sonnet subagent, controller-directed): lowering now panics on non-Func impl members (top-level AND struct-inner), and a leftover-`methods_by_type` assert catches impls targeting non-struct types (enums etc.) — one assert covers all such cases since struct lowering consumes entries via `.remove`. Added should_panic test pinning the enum-impl case. 179 workspace tests green, clippy/fmt clean. Important test gaps noted for follow-up (NOT yet done): control-flow+this Node test, method-calling-another-method Node test, struct-inner func Node test. SLICE 4A REVIEWED + FIXED.

## Slice 3B review
- Review (Slice 3B): independent subagent review (opus). Verdict "with fixes"; NO Critical. Verified sound: guard fall-through, list index/slice math (traced #[a,...mid,b]), ranges, map repr, nested-variant/subexpr preservation, distinct nested-match labels. Fixes in bd526f7e: added Node tests for suffix-after-rest + map patterns (both passed → paths were already correct, just untested); moved binding-union rejection from codegen assert to lowering (pat_binds + descriptive panic, consistent with sibling deferrals) + should_panic test + codegen debug_assert; skipped redundant length>=0 test for rest-only lists. Skipped Minor: union none-collapse test (contrived pattern), gensym _tN reservation (pre-existing). SLICE 3B REVIEWED + FIXED.
- Review (Slice 3A): independent subagent review (opus). Verdict "with fixes"; NO Critical. Core engine verified sound (exhaustiveness backs testless-last-arm; nested/optional-chaining/binding/char all correct). Fixes applied in 0f0e7ef0: should_panic test pinning guard-in-lowering panic; Node tests for nested variant (Wrap(i=A(n))) + match-as-subexpression (IIFE path); stale exhaustiveness doc comment + _s/_r naming drift fixed. Accepted as loud deferrals (not silent miscompiles, consistent with codebase): guards + string/range/tuple/list/map/union patterns panic in lowering. Binding-with-subpattern (name=pat) doesn't parse in match arms → that HirPat::Binding sub branch unreachable from match (harmless). Noted pre-existing (not fixed, affects if/while too): gensym _tN temps not reserved vs user identifiers. SLICE 3A REVIEWED + FIXED.

---
(Older entries below.)

## Slice 2C review
- Review (Slice 2C): independent subagent review (opus). Verdict "with fixes". Critical: field-variant used as first-class value (`let g = Some`) silently miscompiled → FIXED in 124b1ad3 (checker rejects via FieldVariantAsValue; nullary-as-value stays valid). Added tests (reject/accept, qualified-nullary lowering) + deduped tag_obj in emit_enum. Rejected the "shadowing negative test" idea — empirically a variant-named param is a pattern not a binding, so shadowing a variant with a local is unexpressible (invariant vacuously holds). Accepted as known-limitation: positional variant/struct construction panics in lowering (loud ICE, consistent with the codebase's not-yet-implemented deferral pattern). Deferred: equals-via-[TAG] runtime test (needs stdlib linkage, not wired until Slice 5); the tag-identity ABI it relies on IS covered by runs_enum_variant_tag_distinct. SLICE 2C REVIEWED + FIXED.
