# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.7.3] - 2026-07-29

### Fixed

- Stack files instantiated more than once now expand for every instantiation.
  The guard that stops a leaf being emitted twice was keyed on
  `(source, unit.path)`, which sibling `stack` blocks sourcing one shared stack
  file share: the same `stacks/app/terragrunt.stack.hcl` referenced once per
  environment resolves to the same source and declares the same path each time.
  Only the first emitted its leaves. For the rest the recursion produced
  nothing, so the parent shell was kept and every unit beneath it was missing
  from the output. ([#61])

  Emitted entries could therefore be directories that still contain a nested
  `.terragrunt-stack/`. On the monorepo this surfaced on, 10 of 72 entries were
  such shells, hiding 35 leaf directories, and which instantiation expanded
  varied between runs on an identical tree. Consumers driving per-unit
  validate or plan from `--format gha` were silently skipping roughly a third
  of their units, with reruns covering a different subset.

  The guard is now keyed on the materialized unit directory, which is unique
  per instantiation. The original intent is preserved: a leaf reached through
  both discovery and recursion still dedups, because it resolves to one
  directory. Expect more units in the output than 0.7.2 produced.

[#61]: https://github.com/postula/terragrunt-dag/issues/61
[0.7.3]: https://github.com/postula/terragrunt-dag/releases/tag/v0.7.3

## [0.7.2] - 2026-07-29

### Fixed

- Change detection now matches glob watch patterns against changed paths.
  `unit_is_changed` compared each watch entry using a directory prefix or exact
  string equality, and the directory test keys off a `.` in the last component.
  A pattern such as `modules/vpc/**/*.tf*` has last component `*.tf*`, so it was
  classed as an exact file path and compared with `==` against a concrete path,
  which never holds. Glob watch entries therefore matched nothing.

  The failure was silent and partial. Watch entries that are exact file paths
  still matched, so shared and stack-level edits were detected, while module
  sources — which are watched only via globs — were invisible. Under
  `--gha-filter-unchanged` a diff confined to module code produced an empty
  matrix, so consumers validated nothing and reported success. Anyone relying
  on that flag should expect more units to be retained now, which is the
  intended behaviour rather than a regression. ([#59])

  `**` matches zero intermediate directories, so `modules/x/**/*.tf*` matches
  both `modules/x/variables.tf` and `modules/x/sub/main.tf`. A pattern the glob
  parser rejects is treated as a literal path rather than matching everything.

[#59]: https://github.com/postula/terragrunt-dag/issues/59
[0.7.2]: https://github.com/postula/terragrunt-dag/releases/tag/v0.7.2

## [0.7.1] - 2026-07-29

### Fixed

- `--gha-filter-unchanged` now assigns layers after filtering rather than
  before, so the emitted layers are dense from 0. Units that survived the
  filter previously kept their index from the unfiltered DAG, leaving gaps: a
  changed sink stayed at its original layer with nothing below it. Consumers
  that sequence jobs by layer waited on layers that had no work, and
  `--max-layers` was measured against the unfiltered depth, so a single
  changed layer-3 unit reported four buckets and could trip a cap even though
  the real work was one layer deep. Layer numbers in filtered output change,
  and dependencies on filtered-out units are ignored when layering, which is
  the rule already applied to dependency paths that match no emitted unit.
  ([#57])

  Note for consumers with a fixed number of layer jobs: this makes empty
  layers a suffix rather than scattered holes, but does not remove them. A
  diff touching no unit still yields an empty result, as do layers beyond the
  filtered depth. On runners that drop rather than skip a job whose matrix is
  empty, such a layer can break a `needs:` chain.

[#57]: https://github.com/postula/terragrunt-dag/issues/57
[0.7.1]: https://github.com/postula/terragrunt-dag/releases/tag/v0.7.1

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
