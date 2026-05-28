# terragrunt-dag

[![crates.io](https://img.shields.io/crates/v/terragrunt-dag.svg)](https://crates.io/crates/terragrunt-dag) [![License: AGPL-3.0-or-later](https://img.shields.io/badge/license-AGPL--3.0--or--later-blue.svg)](LICENSE) [![CI](https://github.com/postula/terragrunt-dag/actions/workflows/ci.yml/badge.svg)](https://github.com/postula/terragrunt-dag/actions/workflows/ci.yml) [![docs.rs](https://docs.rs/terragrunt-dag/badge.svg)](https://docs.rs/terragrunt-dag) ![MSRV](https://img.shields.io/badge/rust-1.91.1%2B-blue.svg)

Fast dependency graph generator for terragrunt monorepos. Outputs Atlantis, Digger, GitHub Actions matrix, JSON, or YAML.

## Install

```bash
cargo install --path .
```

## Usage

```bash
# Atlantis config
terragrunt-dag ./live/prod --format atlantis > atlantis.yaml

# Digger config
terragrunt-dag ./live/prod --format digger > digger.yaml

# GitHub Actions matrix (with change detection)
terragrunt-dag ./live/prod --format gha --base-ref origin/main > matrix.json

# JSON (for scripting)
terragrunt-dag ./live/prod --format json | jq '.projects[] | .name'
```

## Options

```
terragrunt-dag <ROOT> [OPTIONS]

Options:
  -f, --format <FORMAT>              Output format [json|yaml|atlantis|digger|gha] (default: json)
  -v, --verbose                      Debug output to stderr
      --filter <PATTERN>             Filter projects by glob (e.g., 'prod/*', '**/vpc')
      --workflow <NAME>              Workflow name (default: terragrunt)
      --workspace <NAME>             Override workspace for all projects
      --autoplan                     Enable autoplan (default: true)
      --automerge                    Enable automerge (default: false)
      --parallel-apply               Enable parallel apply (default: false)
      --cascade-dependencies <BOOL>  Include transitive dependencies (default: true)
      --base-ref <REF>               Git ref to diff against for `gha` change detection (e.g., origin/main)
      --gha-filter-unchanged         Drop unchanged units from the `gha` matrix (honors --cascade-dependencies)
      --max-layers <N>               For `gha`: fail non-zero if the DAG needs more than N layer buckets
```

## What it does

1. Finds all `terragrunt.hcl` files
2. Parses `dependency`, `include`, `terraform.source`, `read_terragrunt_config()`, `file()`, `sops_decrypt_file()`
3. Resolves `find_in_parent_folders()`, `get_repo_root()`, path functions
4. Outputs projects with dependencies and watch files

## Output

Atlantis output includes `execution_order_group` for proper dependency ordering:

```yaml
projects:
- name: vpc
  dir: vpc
  workspace: vpc
  workflow: terragrunt
  autoplan:
    when_modified:
    - '**/*.hcl'
    - '**/*.tf'
    - ../../../root.hcl
    - ../../../_modules/vpc/**/*.tf
    enabled: true
  execution_order_group: 0
- name: app
  dir: app
  execution_order_group: 1  # runs after vpc
```

### GitHub Actions / Forgejo Actions matrix

The `gha` format emits a matrix object (`{"include":[...]}`) ready to be consumed by `fromJSON()` in a downstream job. Each entry exposes `name`, `working-directory`, `dependencies`, `layer` (= `execution_order_group`), and `changed`.

```yaml
jobs:
  plan-matrix:
    runs-on: ubuntu-latest
    outputs:
      matrix: ${{ steps.gen.outputs.matrix }}
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0
      - id: gen
        run: |
          echo "matrix=$(terragrunt-dag . --format gha --base-ref origin/main)" >> "$GITHUB_OUTPUT"

  terragrunt:
    needs: plan-matrix
    runs-on: ubuntu-latest
    strategy:
      matrix: ${{ fromJSON(needs.plan-matrix.outputs.matrix) }}
    steps:
      - uses: actions/checkout@v4
      - if: ${{ matrix.changed }}
        working-directory: ${{ matrix.working-directory }}
        run: terragrunt plan
```

`dependencies` and `layer` let callers chain layered jobs via `needs:` instead of per-step `if:` filtering. `--base-ref` requires `git` on PATH (standard in CI); on git failure it warns to stderr and marks all units unchanged. Use `--max-layers <N>` to fail-fast when the DAG exceeds the number of layer-jobs your workflow has hardcoded.

## Performance

Target: <500ms for 800 projects. Uses rayon for parallel processing and caches parsed configs.

## Comparison with terragrunt-atlantis-config

| Feature                         | terragrunt-dag               | terragrunt-atlantis-config                       |
|---------------------------------|------------------------------|--------------------------------------------------|
| **Language**                    | Rust                         | Go                                               |
| **Output formats**              | Atlantis, Digger, JSON, YAML | Atlantis only                                    |
| **execution_order_group**       | Always computed              | Opt-in (`--execution-order-groups`)              |
| **Parallel processing**         | Yes (rayon)                  | No
| **Config caching**              | Yes                          | No                                               |
| **autoplan default**            | true                         | false                                            |
| **create-workspace default**    | Per-project name             | false (uses "default")                           |
| **Dependency cascade**          | Yes (`--cascade-dependencies`) | Yes (`--cascade-dependencies`)                 |
| **locals overrides**            | No                           | Yes (`atlantis_skip`, `atlantis_workflow`, etc.) |
| **extra_atlantis_dependencies** | No                           | Yes                                              |
| **Project markers**             | No                           | Yes (`--project-hcl-files`)                      |
| **Pre-workflow hook**           | Manual setup                 | Documented                                       |
| **preserve-workflows**          | No                           | Yes                                              |
| **apply-requirements**          | No                           | Yes                                              |

### When to use terragrunt-dag

- You need Digger or JSON/YAML output
- You want execution ordering by default
- You want fast builds (Rust binary, config caching)
- Simple setup without per-module overrides

### When to use terragrunt-atlantis-config

- You need per-module locals overrides (`atlantis_skip`, `atlantis_workflow`)
- You need `extra_atlantis_dependencies` for custom watch files
- You need `--apply-requirements` or `--preserve-workflows`

## License

GNU Affero General Public License v3.0 or later (AGPL-3.0-or-later). See [LICENSE](LICENSE) for the full text.
