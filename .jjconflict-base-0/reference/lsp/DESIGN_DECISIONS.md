# LSP Design Decisions

This file tracks design decisions, not implementation details.

## Accepted

1. The LSP will use a shared per-document analysis result as its source of truth instead of separate feature-specific parsing and traversal pipelines.

2. The first-step feature set will prioritize correctness and stable ranges over breadth. In practice, this means better diagnostics, hover, definition, symbols, and semantic tokens before references, rename, or advanced completion.

3. Diagnostics will be sourced from the compiler pipeline as much as possible, rather than maintaining a separate LSP-only diagnostic interpretation layer.

4. `textDocument/documentSymbol` is the initial implementation of "module structure". Breadcrumbs and outline support should come from this before adding custom higher-level module views.

5. Go-to-definition will target exact spans, not just files. File-only navigation is not sufficient for a rust-analyzer-like experience.

6. Cross-file support in the first pass will focus on import targets and imported items, not on a full workspace-global symbol index.

7. Position conversion will be centralized in the document model so every feature uses the same byte/LSP mapping rules.

8. The first diagnostics pass will publish compiler diagnostics for the active document itself. Cross-file diagnostics remain a later extension, but import-based navigation will already cross file boundaries.

9. First-pass go-to-definition will use a lightweight lexical/module resolver in the LSP layer for local bindings and imports, instead of waiting for a full workspace-global symbol index.

10. Imported modules and imported items will be resolved by loading dependency documents on demand from disk rather than pre-indexing the full workspace.

11. The semantic token legend stays stable in this pass; correctness of ranges and coverage takes priority over adding new token classes.

12. Workspace symbols currently search open documents only. That keeps results consistent with live buffer contents and avoids introducing a second, partial workspace index before references/rename work exists.

13. Completion in this phase is scope-aware but intentionally lexical: keywords, top-level declarations, imports, parameters, and earlier local bindings in the active scope are included, without attempting full type-directed or global completion.

14. References search the currently open documents plus any explicitly loaded definition document used during navigation. This keeps reference results tied to the analysis model already in memory.

15. Rename is intentionally narrower than references in this phase: it is enabled only for symbols defined in the active document and is disabled for module targets and cross-file imported symbols until import/export identity is modeled more precisely.

16. Member completion in this phase is analysis-backed rather than text-backed: it appears when the AST parses a member-access expression and the receiver type can be inferred. Incomplete parse recovery for `foo.` style editing is deferred.

17. `textDocument/documentSymbol` now treats declaration and selection ranges as distinct semantics: `range` spans the full declaration container, while `selectionRange` targets the identifier/token that should be focused in outlines and breadcrumbs.

## Open

1. Whether the long-term analysis source should stay on top of the current type checker plus targeted LSP indexing, or move fully onto salsa-backed workspace queries.

2. Whether semantic tokens should remain mostly syntax/AST driven, or evolve toward resolver-backed classification once reference tracking exists.

3. Whether unopened dependency files should be cached permanently in the workspace or loaded on demand for navigation only.
