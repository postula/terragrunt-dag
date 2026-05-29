# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
