# LSP Implementation Plan

## Current Gap Analysis

Compared with `rust-analyzer` and the Gleam language server, the current Nymph LSP is missing or underpowered in these areas:

1. Source-of-truth analysis
The server reparses individual open buffers, but editor features are implemented through separate ad hoc walkers. Diagnostics, hover, definitions, symbols, and tokens do not share one consistent semantic model.

2. Navigation accuracy
`textDocument/definition` is effectively incomplete. Import navigation only returns a file path without a precise span, and local identifiers do not resolve reliably to lexical definitions.

3. Diagnostic fidelity
Parse errors are reduced to strings and published with placeholder ranges. Type errors are partially surfaced, but not through the compiler's richer salsa diagnostics pipeline.

4. Position handling
The server currently mixes byte offsets, Unicode scalar counts, and LSP positions. That will cause incorrect ranges in editors once non-ASCII text appears.

5. Module structure quality
Document symbols exist, but they are mostly isolated from the rest of the analysis model and do not clearly establish the module-level outline as a first-class feature.

6. Feature depth beyond the requested first step
The biggest missing features relative to mature language servers are references, rename, workspace symbols, signature help, completions beyond keywords, code actions, and inlay hints.

## Delivery Phases

### Phase 1: Shared Document Analysis

- Introduce a per-document analysis result that owns:
  - parsed AST
  - compiler diagnostics
  - type-checking context
  - import resolution helpers
  - a line index for byte <-> LSP position conversion
- Route all first-step LSP features through this shared analysis result.
- Prefer compiler queries and type-checker data over handwritten fallback logic where possible.

### Phase 2: Correct Diagnostics

- Replace string-only parse error storage with structured diagnostics.
- Use compiler salsa queries to gather parse and type diagnostics for the current file.
- Preserve cross-file type errors for imported modules when available.
- Publish exact ranges and stable `source` labels.
- Clear stale diagnostics on close or when a document becomes clean.

### Phase 3: Hover Type Information

- Keep the current AST-local hover traversal as a base where it is already useful.
- Rework it to consume the shared analysis result and the new position model.
- Prefer real inferred types from the type checker.
- Fall back to declared annotations or structural descriptions only when inference is unavailable.
- Ensure hover ranges are exact and stable.

### Phase 4: Go-To-Definition

- Introduce a definition target model with:
  - URI
  - exact span
  - symbol kind
- Support, in order:
  - top-level declarations in the current file
  - local lexical bindings in functions, blocks, closures, loops, and match arms
  - imported module files
  - imported items from other modules
- Reuse document analysis for the current file and lazy-load parsed dependency documents for imported modules.

### Phase 5: Module Structure

- Keep `textDocument/documentSymbol` as the primary "module structure" surface.
- Improve symbol extraction so it consistently covers:
  - lets
  - functions
  - type aliases
  - structs and fields
  - enums and variants
  - namespaces
  - interfaces
  - impl blocks
- Make symbol ranges and selection ranges precise using shared position utilities.

### Phase 6: Semantic Tokens

- Keep AST-driven tokenization rather than pure text scanning as the long-term model.
- Remove avoidable keyword-only fallbacks where AST data is available.
- Ensure tokens use the shared position model.
- Preserve existing token types and modifiers for extension compatibility.
- Expand token coverage only after correctness is solid.

## First-Step Success Criteria

The first implementation pass is successful when:

- semantic tokens are returned with stable, editor-correct ranges
- hover shows type information for common declarations and identifier references
- go-to-definition resolves local bindings, top-level declarations, and imports
- document symbols provide a usable module outline
- parse and type errors are reported as document diagnostics with accurate ranges

## Next Features After This Pass

Priority order after the requested first step:

1. references
2. rename
3. workspace symbols
4. completion from scope and imports
5. signature help
6. code actions / quick fixes
7. inlay hints
