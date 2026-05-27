// benches/benchmarks.rs

use camino::Utf8PathBuf;
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use terragrunt_dag::discovery::{discover_files, discover_projects};
use terragrunt_dag::parser::parse_terragrunt_file;
use terragrunt_dag::processor::ParseCache;
use terragrunt_dag::stack;

fn bench_discovery(c: &mut Criterion) {
    let fixture = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/discovery/nested_projects");

    c.bench_function("discover_projects/nested", |b| b.iter(|| discover_projects(black_box(&fixture))));
}

fn bench_parse_hcl(c: &mut Criterion) {
    let fixtures = ["simple_dependency", "multiple_dependencies", "complex"];

    let mut group = c.benchmark_group("parse_hcl");
    for fixture in fixtures {
        let path = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/parser")
            .join(fixture)
            .join("terragrunt.hcl");

        if path.exists() {
            group.bench_with_input(BenchmarkId::new("fixture", fixture), &path, |b, path| {
                b.iter(|| parse_terragrunt_file(black_box(path)))
            });
        }
    }
    group.finish();
}

fn bench_nested_stacks_expand(c: &mut Criterion) {
    let fixture = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/stack/values_nested_stacks");

    if !fixture.exists() {
        return;
    }

    c.bench_function("stack_expand/nested_chain", |b| {
        b.iter(|| {
            let discovered = discover_files(black_box(&fixture)).expect("discover");
            let cache = ParseCache::new();
            stack::expand(discovered.units, &discovered.stack_files, &cache)
        });
    });
}

criterion_group!(benches, bench_discovery, bench_parse_hcl, bench_nested_stacks_expand);
criterion_main!(benches);
