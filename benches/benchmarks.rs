// benches/benchmarks.rs

use camino::Utf8PathBuf;
use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use terragrunt_dag::discovery::discover_projects;
use terragrunt_dag::parser::parse_terragrunt_file;

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

criterion_group!(benches, bench_discovery, bench_parse_hcl);
criterion_main!(benches);
