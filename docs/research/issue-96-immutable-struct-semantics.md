# Issue 96: immutable struct construction, cloning, visibility, and matching

Status: resolved planning decision, 2026-08-17. This note is based on
`language-identity-plan@origin` at `56f05139c160`. It specifies the struct portion of the language
identity; it does not implement it.

## Source contract

Struct construction and patterns use named fields only:

```nymph
let fresh = Foo(p = 1, i = 2, q = 3)
let clone = Foo(...fresh)
let updated = Foo(...fresh, p = 4)

match (updated) {
	Foo(p, i, q) -> complete(p, i, q),
	Foo(p, ...) -> partial(p),
	Foo(...) -> shape_only(),
}
```

A clone/update accepts exactly one source spread. It must be the first argument, its value must have
the exact resolved nominal struct and generic arguments of the result, and every replacement must be
named. Duplicate explicit fields are errors. “Replacement wins” means that an explicit field replaces
the source field; later duplicate replacements do not win.

A struct pattern contains named shorthand fields (`field`), named subpatterns (`field = pattern`),
and an optional trailing anonymous `...`. Positional struct-field patterns are invalid. The `...`
binds nothing and is required whenever any field in the complete structure is omitted, including an
inaccessible field. `Foo()` is therefore complete only for a zero-field struct, while `Foo(...)` is
the shape-only pattern available wherever `Foo` can be named.

## Visibility and defaults

An omitted struct-field visibility is `internal`:

```nymph
public struct Foo(
	public p: int,
	i: int,          // internal
	private q: int,
)
```

Availability is contextual and uses compiler-owned identity:

- `public` fields are available wherever the nominal type is visible.
- `internal` fields require equality of the exact resolved `PackageId`.
- `private` fields require equality of the declaring `ModuleIdentity`.

Fresh construction is available only when every field is available in the source context. Every
available field without a default must be supplied; an available defaulted field may be omitted. A
hidden field blocks fresh construction even when it has a default. Constructor availability is
therefore derived entirely from field availability and default presence, with no separate constructor
modifier.

Defaults have the declaring module's lexical scope and no implicit sibling-field scope. They cannot
refer to earlier, later, or same fields merely by field name. Supplied expressions evaluate exactly
once, left-to-right as written; the nominal owner then evaluates omitted defaults exactly once in
field declaration order. Clone/update never evaluates defaults.

## Three-context matrix

The table assumes that the nominal struct itself is visible in the source context.

| Operation                                      | Outside package               | Same package, other module                | Declaring module |
| ---------------------------------------------- | ----------------------------- | ----------------------------------------- | ---------------- |
| Fresh construction                             | Only if every field is public | Only if every field is public or internal | Yes              |
| Whole-source opaque clone                      | Yes                           | Yes                                       | Yes              |
| Replace a public field                         | Yes                           | Yes                                       | Yes              |
| Replace an internal field                      | No                            | Yes                                       | Yes              |
| Replace a private field                        | No                            | No                                        | Yes              |
| Omit an available defaulted field              | Yes                           | Yes                                       | Yes              |
| Let a hidden default enable fresh construction | Never                         | Never                                     | Not applicable   |
| Access or pattern-bind a public field          | Yes                           | Yes                                       | Yes              |
| Access or pattern-bind an internal field       | No                            | Yes                                       | Yes              |
| Access or pattern-bind a private field         | No                            | No                                        | Yes              |
| Ignore unavailable fields with pattern `...`   | Yes                           | Yes                                       | Yes              |

Whole-source clone/update copies every field, including hidden fields. It evaluates the source once,
then evaluates replacements once from left to right, and leaves the old immutable value unchanged.
Complete equality, hashing, and privileged `echo` use every ordinary field independently of source
context. Field access and pattern binding remain contextual.

## Stable shapes and ownership

The source AST keeps call syntax unresolved because the parser cannot know whether a call target is a
function, struct, or enum variant. Call arguments use an explicit sum rather than a boolean
combination:

```text
CallArgument = Value { name: Option<Name>, value } | Spread { value }
```

Struct patterns likewise retain named field patterns and an explicit `IgnoreRemaining` entry, with no
positional struct-field form.

After name and type resolution, sema owns a stable plan equivalent to:

```text
StructConstructionPlan {
	struct_definition: DefinitionId,
	mode: Fresh | CloneUpdate,
	source: Option<NodeId>,
	explicit_fields: [{ field_definition: DefinitionId, value: NodeId }],
	omitted_default_fields: [DefinitionId],
}
```

Sema owns constructor classification, exact source-type checking, contextual availability, required
and defaulted completion, duplicate detection, pattern omission, and diagnostics. Stable lowering
consumes that checked plan rather than recovering meaning from field-name strings.

Complete and recovered semantic interfaces retain every field in declaration order. Each field keeps
its stable identity, normalized visibility, type, and `has_default`; these facts participate in the
semantic fingerprint. A field's stable identity carries its declaring package/module ownership.
Contextual checking queries availability instead of deleting private or internal fields from the
environment. Public documentation is a separate filtered projection.

Default expression bodies do not enter the exported semantic interface. They remain executable
artifacts owned by the nominal declaring module. Changing a default body rebuilds that runtime owner
without semantically invalidating importers when `has_default` is unchanged; changing default presence
changes the semantic interface.

HIR distinguishes backend-neutral operations equivalent to:

```text
StructFresh { struct_definition, supplied_fields }
StructCloneUpdate { struct_definition, source, replacements }
```

The nominal owner's HIR field shape carries an optional executable default body. Code generation owns
the concrete complete-copy representation and may use unobservable structural sharing. Pattern HIR
contains only checked explicit field subpatterns; the omission marker needs no runtime operation.

## Diagnostics and migration

Diagnostics require a primary operation span, declaration/first-occurrence secondary spans where
available, and contextual notes. Stable categories cover:

- positional struct construction or patterns;
- a source spread that is not first or more than one source spread;
- a source with the wrong nominal type or generic arguments;
- duplicate, unknown, inaccessible, or missing fields;
- fresh construction blocked by inaccessible fields;
- a partial pattern missing trailing `...`;
- inaccessible pattern binding or field access; and
- a default that attempts to use an implicit sibling field.

Diagnostics may name hidden fields: visibility is source access control, not secrecy. A blocked fresh
construction should suggest an opaque clone when a source exists or a public/internal owner function
when the caller needs a fresh value. Safe positional constructions can receive machine-applicable
field labels; moving a spread is not automatically safe because it can change evaluation order.

The migration corpus includes:

```nymph
Point(1, 2)                       // before
Point(x = 1, y = 2)               // after

public struct Point(x: int)       // before: omission leaked outside the package
public struct Point(public x: int) // after: intentionally public

Point(x)                          // before: implicit partial pattern
Point(x, ...)                     // after: explicit omission

Point(x = point.x, y = point.y)   // before: manual reconstruction
Point(...point)                   // after: complete opaque clone
```

Sibling-dependent defaults migrate to an owner function that supplies both fields explicitly. Fresh
construction blocked by hidden fields likewise migrates to an intentional owner function.

## Verification gates

Implementation is complete only when the following gates pass:

1. Parser and formatter tests accept and idempotently format fresh, clone, update, complete-pattern,
   partial-pattern, and shape-only forms; they reject positional fields and invalid spread placement.
2. Complete/recovered interface and fingerprint tests retain every visibility class, normalize omitted
   visibility to `internal`, and distinguish default body changes from default-presence changes.
3. Compile-pass and compile-fail fixtures cover every matrix cell using exact package and module
   identities, including independently resolved same-name/version packages.
4. Stable-plan and HIR snapshots use stable field definitions and distinguish fresh construction from
   clone/update.
5. Node execution traces prove exactly-once, left-to-right source/supplied/replacement evaluation,
   declaration-order defaults, no defaults during clone/update, complete hidden-field preservation,
   and unchanged old values.
6. Diagnostic snapshots cover source-available and interface-only declarations, causal notes, and safe
   migration suggestions.
7. Once the owning feature slices exist, cross-feature tests prove complete hidden-field equality,
   hashing, and `echo`, contextual access/matching, and separately filtered public documentation.
