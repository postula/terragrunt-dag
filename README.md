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
      --max-units-per-layer <N>      For `gha`: split layers holding more than N matrix cells (default: 256, 0 disables)
      --units-per-job <K>            For `gha`: pack K units into one matrix cell (default: 1)
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

### Keeping a wide layer under the matrix cap

GitHub Actions [caps a matrix at 256 jobs per workflow run](https://docs.github.com/en/actions/reference/limits), so a layer wider than that produces a matrix the consumer cannot expand. Units within a layer are mutually independent, so there are two safe ways to narrow one, and they cost different things:

- `--units-per-job <K>` (default `1`) packs K units into one matrix cell. One job runs them back to back. Cell count drops, **depth does not change**.
- `--max-units-per-layer <N>` (default `256`) splits a layer holding more than N cells into `ceil(cells/N)` consecutive layers. This adds a barrier per split and charges the extra depth against `--max-layers`.

Batching is applied first, splitting to whatever is left over. Both chunk by sorted unit name, so a unit lands in the same cell across reruns; consumers keying caches or PR comments on position would otherwise see it move. `--max-layers` is measured after both.

The difference on a real 913-unit tree with layer profile `[414, 238, 83, 67, 42, 40, 22, 7]`:

| flags | depth | cells | widest layer |
|---|---|---|---|
| `--max-units-per-layer 0` (off) | 8 | 913 | 414 |
| `--max-units-per-layer 256` | 9 | 913 | 256 |
| `--max-units-per-layer 100` | 14 | 913 | 100 |
| `--max-units-per-layer 256 --units-per-job 2` | 8 | 458 | 207 |
| `--max-units-per-layer 100 --units-per-job 5` | 8 | 186 | 83 |

Splitting alone at a cap of 100 costs six extra layers; batching gets under the same cap at the original depth. Splitting is the backstop for what batching cannot absorb.

Because the documented cap is per *run* rather than per matrix, a workflow with several layer jobs may want a value below 256.

At `--units-per-job 1` each entry describes one unit, as above. Above 1 a cell no longer maps to a single unit, so `working-directory` and `dependencies` are replaced by a `units` list and `name` becomes a job label:

```json
{"name": "live_prod_vpc (+2 more)", "layer": 0, "changed": true,
 "units": [{"name": "live_prod_vpc", "working-directory": "live/prod/vpc", "dependencies": [], "changed": true}]}
```

The shape is decided by the flag, not per cell, so the matrix never has heterogeneous keys. A cell is `changed` if any unit in it is.

A unit is marked changed if any of its own source files changed, and (with `--cascade-dependencies`, the default) the change is propagated to its downstream dependents through the DAG.

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
