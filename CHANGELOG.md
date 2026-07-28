# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.7.0] - 2026-07-28

### Fixed

- Units from a `stack` block whose source lives elsewhere in the repo are now
  materialized under the parent unit's `.terragrunt-stack/` tree instead of
  under the sourced tree. Recursion previously derived the unit path from the
  child stack file's own directory, so with root `live` a stack sourcing
  `stacks/vault` emitted units at `stacks/vault/.terragrunt-stack/*` — outside
  the requested root. Emitted paths change for these units, and they now stay
  within the root by construction. ([#48])
- Dependency targets for a stack unit resolve from where the unit
  materializes rather than from the shared module directory. Before
  `terragrunt stack generate` had run, a unit's `../backends` resolved to
  `modules/vault/auth/backends` instead of the sibling unit, leaving no edge
  between any two emitted units. Config is still read from the module, the
  only place it exists pre-generation. ([#48])
- Together these produced a matrix in which every unit was reported as
  layer 0 while still declaring dependencies. Consumers that treat the layer
  as an execution barrier ran dependents concurrently with their
  dependencies, so a green run carried no ordering guarantee. ([#48])

### Added

- Dependencies that match no emitted unit are reported instead of being
  silently dropped during layering. A graph in which no edge links two
  emitted units warns by default and fails under `--strict`. It is a warning
  rather than a hard error because `--filter` and `--gha-filter-unchanged`
  legitimately drop dependency targets. ([#48])
- `--verbose` now lists the generated projects on stderr with each unit's
  layer and the dependencies it waits on, so tools that consume stdout
  (Atlantis pre-workflow hooks, CI matrix steps) can see what was generated.
  ([#19])

### Security

- `crossbeam-epoch` 0.9.20 resolves [RUSTSEC-2026-0204]: the `fmt::Pointer`
  impl for `Atomic` and `Shared` dereferenced the underlying pointer, faulting
  on null. Reached transitively through `rayon`.

### Dependencies

- Bumped `camino`, `clap`, `glob`, `hcl-rs`, `rayon`, `serde`, `serde_json`
  and `thiserror`.
- Bumped `actions/checkout` to v7.0.1, `actions-rust-lang/setup-rust-toolchain`
  to v1.17.0, `softprops/action-gh-release` to v3.0.2 and
  `EmbarkStudios/cargo-deny-action` to v2.1.1.
- CI now passes `--locked`, so a `Cargo.lock` that no longer satisfies
  `Cargo.toml` fails the build instead of being silently regenerated.

[#19]: https://github.com/postula/terragrunt-dag/issues/19
[#48]: https://github.com/postula/terragrunt-dag/issues/48
[RUSTSEC-2026-0204]: https://rustsec.org/advisories/RUSTSEC-2026-0204
[0.7.0]: https://github.com/postula/terragrunt-dag/releases/tag/v0.7.0

## [0.6.0] - 2026-05-31

### Added

- `PathExpr::HclExpr` carries unresolved HCL expressions for lazy evaluation.
- `get_repo_root()`, `get_terragrunt_dir()`, and `get_parent_terragrunt_dir()`
  now return real paths instead of empty strings when called from `locals { }`
  or `values = ...` expressions in stack files.
- New `EvalReport` struct aggregates `values_failures`,
  `source_path_failures`, and `file_io_failures` collected during stack
  expansion.
- `--strict` (already existed) now escalates source-path eval failures and
  file-IO stub calls to errors, not just unresolved values.
- Stack `source` references in a parent stack file are now recursively
  followed: `stack "x" { source = "..." }` blocks parse the source's
  `terragrunt.stack.hcl` and emit the inner unit declarations as leaf
  entries. Parent `values = {...}` are bound as `values.X` in the recursed
  scope. Cycle detection and a configurable max recursion depth (default 32)
  prevent infinite loops. Remote stack sources (`git::`, `tfr://`, etc.) are
  skipped with a warning. Output shift: stacks no longer appear in output as
  shell entries; their leaf units do. This is the change that lets
  terragrunt-dag run against a stack-using repo without
  `terragrunt stack generate` first.

### Changed

- `--format gha` (and the other formats) now emit synthetic stack-expanded
  units whose `source` references `${local.x}` or `${values.x}`. Previously
  these units were silently skipped, requiring `terragrunt stack generate` to
  materialize them on disk first. Behavior change: a unit that depended on a
  `local.x`-interpolated source path now appears in output with its
  dependency edges, where it was previously missing.
- `file()`, `read_terragrunt_config()`, `templatefile()`, `jsondecode()`,
  `yamldecode()`, `find_in_parent_folders()`, `get_env()`, `run_cmd()`, and
  similar I/O-style functions are NOT implemented. Calls to them record
  `file_io_failures` and return empty values (or error under `--strict`).
  Real implementations are scheduled for a future release.

[0.6.0]: https://github.com/postula/terragrunt-dag/releases/tag/v0.6.0

## [0.5.0] - 2026-05-29

### Changed

- `--format atlantis` and `--format digger`: each project's `when_modified` /
  `include_patterns` list no longer contains the transitive watch files of its
  dependencies. Per-project file lists will be smaller. Cross-unit triggering
  still fires through Atlantis `depends_on` / `execution_order_group` and the
  Digger equivalent, so consumers relying on the old transitive lists should
  verify their dep edges still cover the trigger paths they expect (typically
  yes).
- `--cascade-dependencies` now actually controls dependency-edge propagation
  on the change-detection path. It was previously a no-op for the changed-set
  computation used by `gha` output and `--gha-filter-unchanged`. Users running
  with `--no-cascade-dependencies` will see a smaller matrix than before,
  containing only directly-changed units.

### Fixed

- Change detection no longer over-flags units. Previously a change to a single
  shared include such as `live/terragrunt.stack.hcl` marked every transitively
  dependent unit as changed, because each unit's `watch_files` set inherited
  the union of all its dependencies' watch files. Now only units whose own
  source files changed are seeded as changed, and the changed-set is
  propagated downstream through DAG edges (gated by `--cascade-dependencies`,
  default on).

[0.5.0]: https://github.com/postula/terragrunt-dag/releases/tag/v0.5.0
