# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
