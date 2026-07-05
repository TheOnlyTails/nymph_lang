# Compiler Feature Plan

This file tracks compiler and tooling features that appear to have syntax or API surface already, but are still missing a proper implementation.

## Priority Order

Recommended implementation order:

1. Interface inheritance and interface specialization
2. Generic methods and generic interface members
3. Namespace typing
4. Pattern matching codegen
5. Range expression codegen
6. Control-flow expression lowering
7. External package imports
8. Name resolution pass
9. LSP semantic and navigation features

## Feature List

### 1. Interface inheritance and interface specialization

Status:
- The parser records `super_interfaces`.
- The parser also records generic arguments for `impl ... as ...`.
- The type checker currently ignores inherited interfaces and ignores the interface generic arguments during impl checking.

Evidence:
- `compiler/src/parser/decl.rs`
- `compiler/src/types/mod.rs`

Implementation plan:
1. Instantiate interface types with supplied generic arguments in `ImplFor`.
2. Merge inherited interface members into the final interface shape.
3. Validate impl members against the instantiated interface, not just the base name.
4. Add tests for inherited members, generic interface specialization, and invalid impls.

Estimate:
- 1 to 2 weeks

### 2. Namespace typing

Status:
- The transpiler emits namespaces as runtime objects.
- The type checker currently stores namespaces as `Type::Module` with empty members.
- That makes namespace member access and import semantics incomplete.

Evidence:
- `compiler/src/types/mod.rs`
- `compiler/src/transpiler/emit.rs`

Implementation plan:
1. Add a namespace-member collection pass in the type checker.
2. Populate `Type::Module` members from declared namespace items.
3. Reuse the same member model for namespace access and imported namespace bindings.
4. Add tests for namespace member access, type lookup, and imports.

Estimate:
- 3 to 5 days

### 3. Generic methods and generic interface members

Status:
- Member bodies are type-checked with generics in scope.
- The stored member function type drops its generics and saves `generics: []`.
- That means generic member signatures are not represented properly after checking.

Evidence:
- `compiler/src/types/mod.rs`

Implementation plan:
1. Preserve resolved generic params in member `Type::Function` values.
2. Make member-call inference honor those generics.
3. Support explicit generic instantiation on member calls where syntax allows it.
4. Add tests for generic methods on structs, enums, namespaces, and interfaces.

Estimate:
- 3 to 5 days

### 4. External package imports

Status:
- Project, current, and parent imports resolve.
- Package imports are explicitly rejected as unsupported.

Evidence:
- `compiler/src/types/mod.rs`
- `compiler/src/queries.rs`

Implementation plan:
1. Define package metadata and dependency resolution rules.
2. Resolve package roots during type checking and bundling.
3. Decide how package imports map to emitted JS imports.
4. Add diagnostics for missing or ambiguous packages.
5. Add integration tests covering package imports in both typecheck and bundle flows.

Estimate:
- 1 to 2 weeks

### 5. Name resolution pass

Status:
- The dedicated resolver module is still commented scaffolding.
- Symbol identity is still derived ad hoc during type checking and LSP analysis.

Evidence:
- `compiler/src/resolver/mod.rs`

Implementation plan:
1. Introduce a real resolver pass with scope tracking and stable symbol IDs.
2. Resolve local, member, and imported names before type checking.
3. Feed resolved symbol data into type checking and diagnostics.
4. Reuse the same data in LSP hover, definition, reference, and rename features.

Estimate:
- 1 to 2 weeks

### 6. Range expression codegen

Status:
- Range expressions are parsed and typed.
- Transpilation is still a stub that emits an empty array.

Evidence:
- `compiler/src/types/mod.rs`
- `compiler/src/transpiler/emit.rs`

Implementation plan:
1. Define runtime semantics for every range form.
2. Add a runtime helper or stdlib helper for range construction and iteration.
3. Lower every parsed range form to the helper.
4. Add transpiler tests for open, closed, and one-sided ranges.

Estimate:
- 2 to 4 days

### 7. Pattern matching codegen

Status:
- Type checking supports list, tuple, map, range, and string patterns.
- Transpilation only implements a subset.
- Unsupported patterns currently fall back to `true`, which is semantically wrong.

Evidence:
- `compiler/src/types/mod.rs`
- `compiler/src/transpiler/emit.rs`

Implementation plan:
1. Implement recursive runtime checks for list, tuple, map, range, and string patterns.
2. Extend binding extraction for nested matches using those pattern forms.
3. Make `match` and `is`/`!is` share the same pattern-lowering logic.
4. Add tests for successful matches, failed matches, nested bindings, and guards.

Estimate:
- 1 to 2 weeks

### 8. Control-flow expression lowering

Status:
- The AST supports break values and labels on several expression forms.
- Transpilation drops break values.
- Labels on `while` and block expressions are not handled consistently.

Evidence:
- `compiler/src/transpiler/emit.rs`

Implementation plan:
1. Decide the runtime model for loop and block expressions in JS.
2. Preserve `break value` semantics through temp-result lowering or equivalent.
3. Handle labeled loops and labeled blocks consistently.
4. Add tests for nested labeled control flow and expression-valued breaks.

Estimate:
- 3 to 5 days

### 9. LSP semantic and navigation features

Status:
- The server advertises several capabilities beyond what it really implements.
- Semantic tokens currently return an empty token stream.
- Completion is keyword-only.
- Incremental sync applies edits with a naive single-line replacement.

Evidence:
- `lsp/src/server.rs`
- `lsp/src/document.rs`

Implementation plan:
1. Recompute analyzer state after document changes.
2. Emit real semantic tokens from AST and type data.
3. Replace the incremental edit logic with proper LSP range application.
4. Improve completion using visible symbols and type context.
5. Build definition and reference features on top of resolved symbol identities.

Estimate:
- About 1 week

## Notes

- This list is based on a source audit of the current tree.
- It focuses on features that appear partially implemented or structurally incomplete, not on general future ideas.
