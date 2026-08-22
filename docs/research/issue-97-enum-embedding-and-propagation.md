# Issue 97: enum embedding and deep propagation

This note records the decision evidence and compiler ownership for
[issue #97](https://github.com/TheOnlyTails/nymph_lang/issues/97). It is planning material, not an
implementation.

## Decision

Enum embedding is set inclusion, not wrapper construction. An enum has a nominal static view and a
canonical deduplicated set of accepted single-variant types. A value always retains its original
variant identity and fields. Assignment, argument passing, returning, and `as` can give that value a
destination view when its statically known set is included in the destination set.

Every qualified variant is a regular type. Whole embedding adds a source enum's complete set;
selected embedding adds one variant type. Set expansion uses a least fixed point, so self-embedding,
cycles, diamonds, and duplicate paths are harmless. Module-spanning cycles rely on the roadmap's
cyclic-module interface analysis.

Generic single-variant identity retains only parameters used by the variant's fields. Thus
`Option<int>.Some` and `Option<string>.Some` differ, while every instantiation's `Option.None` is one
type and runtime singleton. Context can retain an enclosing enum view when a method needs a generic
argument that the variant identity erases.

Embedding generates no `Into`. Built-in assignability powers contextual coercion and `as`; explicit
`Into` implementations remain legal and power `.to()`. Result propagation first uses direct set
assignability, then falls back to one unique pure, infallible explicit `Into`. Direct propagation
rebuilds the destination `Result.Error` wrapper but leaves the error value unchanged.

Patterns name original qualified variants. A successful pattern binds the source view; an unrefined
binder retains the destination view. Exhaustiveness and duplicate-arm analysis use the final set.
The removed `...Source` pattern and `Destination(source)` construction receive focused diagnostics.

Equality is permitted when enum sets overlap. It compares stable original variant identity and fields;
different identities are false and static views are irrelevant. Hashing uses the same identity and
fields. Runtime identity includes only generic arguments used by that variant.

## Stable semantic shape

The semantic model needs these concepts; exact Rust names remain an implementation choice:

- A single-variant type: stable variant `DefinitionId`, relevant generic arguments, and field shape.
- An enum view: nominal enum `DefinitionId`, generic arguments, and final canonical variant set.
- A refinement: a proven single-variant type plus any enclosing contextual view needed for method
  inference.
- A propagation plan: direct assignability or one selected explicit `Into` implementation.

Stable module interfaces serialize the final sorted variant set and relevant generic projection.
Those shapes participate in interface fingerprints and importer invalidation. The source enum's module
alone owns and emits each variant factory, tag, and field ABI. A destination owns only its native
variants, methods, static type object, and semantic set.

## Compiler ownership

- **Parser/formatter:** whole and selected embedding declarations, single-variant types, ordinary
  qualified patterns, and errors for removed construction/spread-pattern forms.
- **Sema:** fixed-point set expansion, generic projection, assignability, overlap, refinement,
  exhaustiveness, static method resolution, equality availability, and `?` route selection.
- **Stable interfaces:** canonical sets, variant field shapes, generic projection, method owner, and
  fingerprints. Cyclic module groups must collect declarations before solving their sets.
- **HIR:** a backend-neutral enum-view operation for explicit and contextual views; statically selected
  canonical method targets; propagation plans that rebuild failure wrappers and optionally call an
  explicit `Into`.
- **JavaScript lowering:** erase enum-view operations, emit no destination variant aliases, reify only
  relevant generic arguments in variant identity, and use the selected static type object for generic
  receiver dispatch.

The current compiler already has stable enum/variant `DefinitionId`s, canonical enum emission,
tag-based matching, implementation coherence, propagation annotations, and stable HIR lowering. It
does not yet have single-variant types, enum-set interfaces, conversion-aware propagation, or static
receiver dispatch. Current project analysis rejects import cycles before semantic interfaces are
built, so cyclic-module groundwork is a prerequisite for module-spanning embedding fixed points.

## Diagnostics

- Reject malformed or non-enum embedding members.
- When a general enum does not fit selected coverage, list uncovered variants and point to the needed
  refinement, annotation, or explicit conversion.
- Explain when an erased generic argument requires an enclosing view.
- Reject equality with no possible overlapping variant.
- Diagnose duplicate/unreachable variant arms and missing final-set cases.
- Replace `Destination(source)` with contextual assignment or `source as Destination` guidance.
- Replace `...Source` patterns with qualified variant alternatives where practical.
- For `?`, distinguish missing, ambiguous, fallible, and effectful explicit `Into` fallbacks.
- Do not diagnose self-embedding, cycles, diamonds, or repeated paths merely for repetition.

Diagnostics follow the project standard: primary operation span, secondary declaration/refinement
spans, a plain-language cause, and machine-applicable suggestions only when the replacement is unique.

## Verification gates

1. Parser and formatter snapshots cover embedding, single-variant types, casts, and removed forms.
2. Sema tests cover self, mutual, diamond, transitive, repeated, selected, and generic set expansion.
3. Identity tests distinguish used generic parameters and erase unused ones.
4. Stable-interface snapshots verify canonical ordering, fingerprints, importer invalidation, and
   cyclic-module integration.
5. Assignability tests cover annotations, arguments, returns, casts, and pattern refinements.
6. Concrete and generic/interface tests prove that static views select methods.
7. Match tests cover source rebinding, destination-view wildcards, duplicate arms, and exhaustiveness.
8. HIR snapshots cover erased view operations, canonical method targets, direct propagation, and
   explicit-`Into` propagation.
9. Node tests prove one source factory/tag, no destination duplication, generic runtime identity,
   equality, and hash agreement.
10. Deep-`?` tests cover transitive inclusion, single-variant errors, explicit fallback, ambiguity,
    and effect rejection.
11. Removed-form diagnostics receive focused snapshots.
