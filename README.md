# terragrunt-dag

Fast dependency graph generator for terragrunt monorepos. Outputs Atlantis, Digger, JSON, or YAML.

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

# JSON (for scripting)
terragrunt-dag ./live/prod --format json | jq '.projects[] | .name'
```

## Options

```
terragrunt-dag <ROOT> [OPTIONS]

Options:
  -f, --format <FORMAT>              Output format [json|yaml|atlantis|digger] (default: json)
  -v, --verbose                      Debug output to stderr
      --filter <PATTERN>             Filter projects by glob (e.g., 'prod/*', '**/vpc')
      --workflow <NAME>              Workflow name (default: terragrunt)
      --workspace <NAME>             Override workspace for all projects
      --autoplan                     Enable autoplan (default: true)
      --automerge                    Enable automerge (default: false)
      --parallel-apply               Enable parallel apply (default: false)
      --cascade-dependencies <BOOL>  Include transitive dependencies (default: true)
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

MIT
