use criterion::{black_box, criterion_group, criterion_main, Criterion};
use umbral_pep440::{PackageName, Version, VersionSpecifiers};

/// 100 version strings of varying complexity.
fn version_strings() -> Vec<&'static str> {
    vec![
        // Simple versions (20)
        "0.1.0",
        "1.0.0",
        "2.3.4",
        "10.20.30",
        "0.0.1",
        "1.0",
        "2.0",
        "3.1.4",
        "99.99.99",
        "1.2.3.4",
        "0.9.0",
        "5.0.0",
        "1.1.1",
        "2.2.2",
        "3.3.3",
        "4.4.4",
        "5.5.5",
        "6.6.6",
        "7.7.7",
        "8.8.8",
        // Pre-release versions (20)
        "1.0.0a1",
        "1.0.0a2",
        "1.0.0b1",
        "1.0.0b2",
        "1.0.0rc1",
        "2.0.0a1",
        "2.0.0b1",
        "2.0.0rc1",
        "2.0.0rc2",
        "3.0.0a5",
        "1.0a1",
        "2.0b3",
        "3.0rc1",
        "4.0.0alpha1",
        "5.0.0beta2",
        "1.0.0.a1",
        "2.0.0.b1",
        "3.0.0.rc1",
        "1.0.0a0",
        "1.0.0rc99",
        // Post-release versions (20)
        "1.0.0.post1",
        "1.0.0.post2",
        "2.0.0.post1",
        "3.0.0.post10",
        "1.0.post1",
        "1.0.0.post0",
        "1.0.0post1",
        "2.0.0-1",
        "3.0.0.post100",
        "4.0.0.post5",
        "1.1.0.post1",
        "2.2.0.post2",
        "3.3.0.post3",
        "4.4.0.post4",
        "5.5.0.post5",
        "6.0.0.post1",
        "7.0.0.post1",
        "8.0.0.post1",
        "9.0.0.post1",
        "10.0.0.post1",
        // Dev versions (20)
        "1.0.0.dev1",
        "1.0.0.dev2",
        "2.0.0.dev1",
        "3.0.0.dev0",
        "1.0.dev1",
        "1.0.0.dev100",
        "2.0.0.dev5",
        "3.0.0.dev99",
        "4.0.0.dev1",
        "5.0.0.dev1",
        "1.0.0a1.dev1",
        "1.0.0b1.dev2",
        "1.0.0rc1.dev1",
        "2.0.0a1.dev5",
        "3.0.0b2.dev1",
        "1.0.0.post1.dev1",
        "2.0.0.post2.dev3",
        "1.0.dev0",
        "2.0.dev99",
        "3.0.dev1",
        // Epoch versions (20)
        "1!0.0.0",
        "1!1.0.0",
        "1!2.0.0",
        "2!1.0.0",
        "1!1.0.0a1",
        "1!1.0.0.post1",
        "1!1.0.0.dev1",
        "1!1.0.0rc1",
        "2!3.0.0",
        "1!0.1.0",
        "1!1.2.3",
        "1!2.3.4.5",
        "2!0.0.1",
        "1!10.0.0",
        "1!1.0.0b1",
        "1!1.0.0.post1.dev1",
        "3!1.0.0",
        "1!99.0.0",
        "2!2.0.0a1",
        "1!5.0.0.post3",
    ]
}

fn bench_version_parsing(c: &mut Criterion) {
    let versions = version_strings();

    c.bench_function("parse_100_versions", |b| {
        b.iter(|| {
            for v in &versions {
                let _ = black_box(v.parse::<Version>());
            }
        })
    });
}

fn bench_specifier_matching(c: &mut Criterion) {
    // Pre-parse 100 versions
    let versions: Vec<Version> = version_strings()
        .iter()
        .filter_map(|v| v.parse::<Version>().ok())
        .collect();

    let specifier: VersionSpecifiers = ">=1.0,<2.0".parse().unwrap();

    c.bench_function("specifier_match_100_versions", |b| {
        b.iter(|| {
            for v in &versions {
                let _ = black_box(specifier.contains(v));
            }
        })
    });

    let complex_specifier: VersionSpecifiers = ">=1.0.0a1,!=1.0.0b1,<2.0.0.post1".parse().unwrap();

    c.bench_function("complex_specifier_match_100_versions", |b| {
        b.iter(|| {
            for v in &versions {
                let _ = black_box(complex_specifier.contains(v));
            }
        })
    });
}

fn bench_specifier_parsing(c: &mut Criterion) {
    let specifiers = vec![
        ">=1.0",
        "<2.0",
        ">=1.0,<2.0",
        "~=1.4.2",
        "==1.0.*",
        "!=1.5.0",
        ">=1.0,!=1.3.0,<2.0",
        ">=1.0a1,<2.0.0.post1",
        "===exact-version",
        ">1.0,<=3.0",
    ];

    c.bench_function("parse_10_specifiers", |b| {
        b.iter(|| {
            for s in &specifiers {
                let _ = black_box(s.parse::<VersionSpecifiers>());
            }
        })
    });
}

fn bench_package_name_normalization(c: &mut Criterion) {
    let names = vec![
        "requests",
        "Flask",
        "Django",
        "my-package",
        "my_package",
        "My.Package",
        "UPPER_CASE",
        "MixedCase-Package",
        "a",
        "a-b-c-d-e-f",
        "numpy",
        "scikit-learn",
        "scikit_learn",
        "Scikit.Learn",
        "python-dateutil",
        "python_dateutil",
        "PyYAML",
        "setuptools",
        "pip",
        "wheel",
    ];

    c.bench_function("normalize_20_package_names", |b| {
        b.iter(|| {
            for name in &names {
                let _ = black_box(PackageName::new(*name));
            }
        })
    });
}

criterion_group!(
    benches,
    bench_version_parsing,
    bench_specifier_matching,
    bench_specifier_parsing,
    bench_package_name_normalization,
);
criterion_main!(benches);
