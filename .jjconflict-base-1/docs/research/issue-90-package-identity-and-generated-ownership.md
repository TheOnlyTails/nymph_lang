# Issue 90: package identity and generated implementation ownership

Status: resolved planning decision, 2026-08-17. This note records the repository evidence and the
agreed contract for `internal` visibility and generated implementation ownership. It does not
implement package resolution, field access rules, enum embedding, or derived implementations.

## Current seams and gaps

- `nymph-project` parses package names, versions, dependency selectors, and source roots, but
  deliberately owns no compiler identity or Salsa state (`crates/nymph-project/src/lib.rs:1-63`,
  `161-276`).
- `CompilerSession` currently keys source inputs by caller-provided `(ProjectId, ModulePath)`.
  LSP project IDs are filesystem-derived while one-shot compilation uses a constant facade ID, so
  `ProjectId` is a lifecycle/isolation key rather than canonical package membership
  (`crates/nymph-compiler/src/project/session.rs:16-79`, `121-206`;
  `crates/nymph-lsp/src/compiler_state.rs:1572-1601`;
  `crates/nymph-compiler/src/project/mod.rs:237-250`).
- Semantic `ModuleIdentity` is embedded in every stable `DefinitionId`; complete and recovered module
  interfaces hash that identity, their field shapes, and implementation shapes. This is the existing
  propagation and incremental-fingerprint seam (`crates/nymph-sema/src/identity.rs:7-23`, `290-421`;
  `crates/nymph-sema/src/interface.rs:381-405`, `457-753`).
- `internal` is parsed and retained in field/interface shapes, but current interface/environment
  filtering treats every visibility except `private` as generally importable. Package-sensitive
  availability is not implemented (`crates/nymph-ast/src/decl.rs:102-106`;
  `crates/nymph-sema/src/interface_extract.rs:178-180`;
  `crates/nymph-sema/src/environment.rs:836-901`).
- Import resolution distinguishes project-local modules and importable `std`, but rejects other
  package roots. The canonical graph rejects cycles and checks each module from dependency interfaces
  rather than dependency bodies (`crates/nymph-compiler/src/project/resolve.rs:27-127`;
  `crates/nymph-compiler/src/project/queries.rs:2372-2665`, `2779-2965`).
- Implementations already have one stable owning module, are carried by that module's interface, and
  are collected from dependency-interface closures. Runtime/link planning resolves exact stable
  definitions and module owners (`crates/nymph-sema/src/interface.rs:477-617`;
  `crates/nymph-sema/src/environment.rs:134-255`;
  `crates/nymph-compiler/src/project/link_plan.rs:31-281`).
- The standard library's current `Option`/`Result` cross-API uses a third module specifically to avoid
  a mutual import cycle (`stdlib/src/convert.nym:1-31`). Generated embedding conversions need a
  directional canonical owner rather than duplicated source/destination ownership.

## Resolution

### Exact resolved package instances

`PackageId` is a compiler-owned identity for one exact node in a project's resolved dependency graph.
Declared package names, versions, dependency aliases, and source selectors participate in resolution,
but are not themselves the visibility identity.

- Aliases resolving to the same dependency node share one `PackageId`.
- Independently resolved copies have distinct IDs even if name and version match.
- Source/body edits preserve the ID; a resolution-changing manifest edit replaces the affected graph
  node identity and invalidates its semantic dependants.
- Manifestless and standalone inputs receive isolated synthetic package instances.
- Importable standard-library and compiler-ambient definitions retain reserved identity domains rather
  than masquerading as user packages.

`ProjectId` remains the session/workspace lifecycle boundary. Project module inputs become
conceptually `(ProjectId, PackageId, ModulePath)`, while semantic `ModuleIdentity` carries package
identity. Consequently imported definition IDs, implementation IDs, complete/recovered interfaces,
interface fingerprints, runtime owners, and canonical emitted module keys preserve package ownership.
Package-root imports resolve through the package graph to `(PackageId, ModulePath)`; `@/` and relative
imports remain within the current package.

`internal` access requires equal package identity. `private` access additionally requires equal module
identity. Stable compiler interfaces retain complete field structure, visibility, and declaring owner
even when fields are unavailable at the importing source location. Contextual environment/checker
projection enforces availability; public documentation is a separate filtered projection. This keeps
opaque cloning and complete equality/hash possible without granting hidden access or pattern binding.

### Directional generated implementation ownership

A generated `Into<Destination> for Source` implementation is owned by the destination enum's declaring
module. This applies to direct, selected-variant, and transitive whole-enum conversions. The embedding
declaration already gives the destination a dependency on the source, so destination ownership adds no
reverse edge and cannot create the source/destination import cycle that source ownership would require.
A consumer that can name the destination already reaches the owner interface that supplies the
implementation.

Single-type derived implementations are owned by the nominal type's declaring module. Every generated
implementation appears exactly once and receives a structural stable identity derived from the
derivation kind and canonical participating interface/type identities. It participates in the owner
module's complete/recovered interface and structural fingerprint. Implementations are not copied into
both nominal modules and are not placed in an implicitly imported package-wide synthetic module.

This owner rule is compatible with single canonical type emission: source and destination nominal
runtime definitions remain canonical, while exact generated implementation artifacts and their stable
dependencies are delivered from their one semantic owner.

## Incremental and rollout consequences

- Package descriptors and the resolved package graph must become tracked compiler inputs before
  package-sensitive field checking or third-party imports land.
- A package-identity or resolution change invalidates module identities and dependent interfaces;
  body-only edits can continue to backdate structurally unchanged interfaces.
- Emission/bundling maps must key modules by package identity plus module path rather than path alone.
- Interface extraction must preserve private/internal field shapes; environment construction must stop
  treating `internal` as universally available.
- Generated implementations enter ordinary coherence, stable-interface, lowering, and emission paths
  under their canonical owner rather than through a parallel synthesis registry.

## Deliberately unresolved here

- Package-manager, lockfile, registry, and dependency-source syntax.
- Enum embedding parser/HIR representation and path-normalization algorithms.
- Explicit-versus-generated implementation conflict diagnostics.
- The detailed struct/privacy, `echo`, equality/hash, and deep-`?` implementation rollout.
