# Toolchain

This document tells an agent (or a new human contributor) exactly which tools
are needed to work in the Nymph compiler repo, how to install them, and which
commands to run. Versions below reflect what is pinned in the repo and what CI
uses — prefer the pinned versions over "latest".

The repo has **two independent toolchains**:

- **Rust** — the compiler itself (`crates/`), edition 2024, resolver 3.
- **Node + pnpm** — the peripheral JS surfaces: the VitePress docs site
  (`docs/`) and the VS Code extension (`extension/`).

They are separate, with one important overlap: **Node is also a runtime
dependency of the Rust test suite**, because `nymph-codegen` emits JavaScript
and runs it under `node` to verify the output. You cannot fully test the
compiler without Node on `PATH`.

Version control is **Jujutsu (`jj`)**, colocated over a Git backend.

---

## 1. Rust (compiler)

The Rust toolchain is **pinned to `nightly`** via `rust-toolchain.toml`:

```toml
[toolchain]
channel = "nightly"
components = ["rustfmt", "clippy", "rust-src"]
```

Because the channel is pinned in-tree, `rustup` selects it automatically the
first time you run any `cargo` command inside the repo — you do **not** pass
`+nightly` manually. Nightly is required (the workspace uses nightly-only
dependency features, e.g. `chumsky`'s `nightly` feature).

### Install

Install `rustup`, then let it pick up the pinned channel:

```sh
# Install rustup (https://rustup.rs)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Inside the repo, this triggers install of the pinned nightly + components:
cargo --version
```

If the components ever fail to resolve, install them explicitly:

```sh
rustup component add rustfmt clippy rust-src --toolchain nightly
```

### Everyday commands

| Task            | Command                                        |
| --------------- | ---------------------------------------------- |
| Build           | `cargo build`                                  |
| Type-check only | `cargo check`                                  |
| Format          | `cargo fmt`                                     |
| Lint            | `cargo clippy --all-targets --all-features`    |
| Docs            | `cargo doc --no-deps`                          |

**Formatting rules** are enforced by `rustfmt.toml` (hard tabs, 2-space width)
and `clippy.toml` (`allow-mixed-uninlined-format-args = false`, i.e. inlined
format args are required). Run `cargo fmt` before committing.

---

## 2. cargo-nextest (test runner)

`cargo-nextest` is the primary test runner for the compiler. It runs tests in
parallel with better output than `cargo test`.

### Install

```sh
# Prebuilt binary (fastest):
cargo binstall cargo-nextest --secure
# …or build from source:
cargo install cargo-nextest --locked
```

(See <https://nexte.st/docs/installation/pre-built-binaries/> for direct
downloads that skip a compile.)

### Commands

| Task                       | Command                                              |
| -------------------------- | ---------------------------------------------------- |
| All tests                  | `cargo nextest run`                                  |
| One package                | `cargo nextest run -p nymph-sema`                    |
| Filter by name             | `cargo nextest run -E 'test(type_display)'`          |

> **Note:** nextest does not run doctests. If a crate has doctests, also run
> `cargo test --doc`.

`cargo test` still works and is what the CI `Rust` workflow currently runs
(`cargo test --verbose`); nextest is the preferred local runner. Module- and
single-test forms with plain cargo:

```sh
cargo test --lib types::tests                       # a module
cargo test --lib types::tests::test_type_display    # a single test
```

Remember Node must be installed for codegen tests to pass (see §4).

---

## 3. bacon (watch runner, optional but recommended)

`bacon` is a background watcher configured by `bacon.toml`. It re-runs a job on
every file change, which is the fastest inner loop for compiler work.

### Install

```sh
cargo binstall bacon        # or: cargo install bacon --locked
```

### Jobs (defined in `bacon.toml`)

| Command             | What it runs                                              |
| ------------------- | -------------------------------------------------------- |
| `bacon`             | default job: `cargo check`                               |
| `bacon check-all`   | `cargo check --all-targets`                              |
| `bacon clippy-all`  | `cargo clippy --all-targets`                             |
| `bacon test`        | `cargo test` (pass filters after `--`)                   |
| `bacon nextest`     | `cargo nextest run` with nextest analyzer                |
| `bacon doc-open`    | `cargo doc --no-deps --open`                             |
| `bacon run`         | `cargo run`                                              |

Inside the TUI, `c` is bound to `clippy-all`.

---

## 4. Node.js + pnpm (docs, extension, and codegen test runtime)

### Node

Node is required for two reasons:

1. The **codegen crate** shells out to `node` to execute the JavaScript it
   emits, as part of the Rust test suite.
2. The **docs site** and **VS Code extension** are Node/TypeScript projects.

Use a current Node LTS or newer (development is on Node 24+). Verify `node` is
on `PATH`:

```sh
node --version
```

### pnpm

The package manager is **pnpm**, pinned via the root `package.json`
`packageManager` field (`pnpm@11.11.0`). The easiest way to get the exact
version is Corepack, which ships with Node:

```sh
corepack enable
corepack prepare --activate     # installs the pinned pnpm from package.json
```

(Alternatively `npm install -g pnpm`, but Corepack guarantees the pinned
version. Note: `extension/package.json` pins its own `pnpm@10.28.1` for that
sub-package — Corepack handles this per-directory automatically.)

### Workspace layout

`pnpm-workspace.yaml` declares two workspace packages:

- `docs/` — the VitePress documentation site.
- `extension/` — the VS Code extension.

Install everything from the repo root:

```sh
pnpm install
```

### JS tooling (oxc-based)

This repo uses the **oxc** toolchain for JS/TS rather than Prettier/ESLint.
Root `package.json` scripts:

| Task   | Command       | Underlying tool                        |
| ------ | ------------- | -------------------------------------- |
| Format | `pnpm format` | `oxfmt`                                |
| Lint   | `pnpm lint`   | `oxlint --type-aware --type-check`     |

Per-package scripts:

```sh
# Docs (VitePress)
pnpm --filter nymph-docs dev        # local dev server
pnpm --filter nymph-docs build

# VS Code extension
pnpm --filter nymph compile         # tsc -p ./
pnpm --filter nymph watch
```

---

## 5. Jujutsu (jj) — version control

The repo is version-controlled with **Jujutsu**, colocated over a Git backend
(there is both a `.jj/` and a `.git/` directory). The remote `origin` is the
GitHub repo, so Git tooling and GitHub PRs work as usual; `jj` is the
day-to-day front-end.

### Install

```sh
cargo binstall jj-cli       # or: cargo install jj-cli --locked
# macOS: brew install jj  |  see https://jj-vcs.github.io/jj/latest/install-and-setup/
```

Development is on `jj` 0.43+. Verify:

```sh
jj --version
```

### Minimal workflow

Because the repo is colocated, you can use either `jj` or `git`, but prefer
`jj`:

```sh
jj st                       # working-copy status
jj log                      # commit graph
jj describe -m "message"    # set the current change's description
jj new                      # start a new change on top
jj git fetch                # sync from origin
jj git push                 # push the current branch/bookmark
```

If you are more comfortable with Git, `git status` / `git commit` / `git push`
also operate on the same store thanks to colocation.

---

## Quick start (fresh machine)

```sh
# 1. Rust (auto-selects pinned nightly from rust-toolchain.toml)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# 2. Cargo tooling
cargo binstall cargo-nextest bacon jj-cli

# 3. Node + pnpm
corepack enable && corepack prepare --activate

# 4. Install JS deps
pnpm install

# 5. Verify everything
cargo build
cargo nextest run          # requires `node` on PATH for codegen tests
cargo clippy --all-targets --all-features
pnpm lint
```

## Agent skills

### Issue tracker

Issues and PRDs are tracked as GitHub issues (`gh` CLI) in `TheOnlyTails/nymph_lang`. See `docs/agents/issue-tracker.md`.

### Triage labels

Default five-role vocabulary, each label string equal to its name. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: `CONTEXT.md` + `docs/adr/` at the repo root. See `docs/agents/domain.md`.

### Commit discipline

Commit completed work in coherent, reviewable units aligned with the GitHub
issues being implemented. Each commit must represent one whole logical unit of
work: do not combine unrelated issues in one commit, and do not split a single
behavioral change into commits that are incomplete or fail their relevant
checks on their own. If an issue is too large for one commit, split it only at
explicit, independently valid implementation boundaries and identify the issue
in every commit message.

### Standard library ownership

Prefer implementing standard-library behavior in Nymph whenever doing so is
straightforward. Keep JavaScript externals for host/runtime primitives and
other behavior that cannot reasonably be expressed in Nymph, so users can
inspect ordinary stdlib behavior without reading the external JavaScript.

### Completion summaries

When finishing implementation work, include a file-by-file summary of every
changed file so the user can review the working-copy diff efficiently. For each
file, state what changed and why; group generated or purely mechanical changes
only when reviewing them individually would add no useful information. Also
report verification results and any known failures separately from the file
summary.
