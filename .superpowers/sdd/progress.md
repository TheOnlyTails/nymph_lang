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
- SLICE 2B COMPLETE. PENDING: independent whole-branch review of Slice 2A+2B (all implemented inline, no independent review).
