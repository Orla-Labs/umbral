use criterion::{black_box, criterion_group, criterion_main, Criterion};
use umbral_pep440::Version;
use umbral_pep508::Requirement;
use umbral_resolver::mock::MockRegistry;
use umbral_resolver::{resolve, ResolverConfig};

fn make_config() -> ResolverConfig {
    ResolverConfig {
        python_version: "3.12.0".parse::<Version>().unwrap(),
        markers: None,
        pre_release_policy: Default::default(),
    }
}

/// Build a small dependency graph with 5 packages:
///   app -> web-framework >=1.0, db-driver >=2.0
///   web-framework 1.0.0 -> utils >=0.5
///   web-framework 1.1.0 -> utils >=0.6
///   db-driver 2.0.0 -> utils >=0.5
///   db-driver 2.1.0 -> utils >=0.5
///   utils 0.5.0, 0.6.0, 0.7.0 (no deps)
///   logging 1.0.0, 1.1.0 (no deps, pulled by web-framework 1.1.0)
fn build_small_registry() -> MockRegistry {
    let mut registry = MockRegistry::new();

    // utils -- leaf package
    registry
        .add_version("utils", "0.5.0", vec![])
        .add_version("utils", "0.6.0", vec![])
        .add_version("utils", "0.7.0", vec![]);

    // logging -- leaf package
    registry
        .add_version("logging", "1.0.0", vec![])
        .add_version("logging", "1.1.0", vec![]);

    // web-framework
    registry
        .add_version("web-framework", "1.0.0", vec![("utils", ">=0.5")])
        .add_version(
            "web-framework",
            "1.1.0",
            vec![("utils", ">=0.6"), ("logging", ">=1.0")],
        );

    // db-driver
    registry
        .add_version("db-driver", "2.0.0", vec![("utils", ">=0.5")])
        .add_version("db-driver", "2.1.0", vec![("utils", ">=0.5")]);

    registry
}

/// Build a medium dependency graph with 20 packages arranged in layers:
///   Layer 0 (root deps): app-core, app-web, app-api
///   Layer 1: framework, http-client, serializer, validator, cache
///   Layer 2: connection-pool, encoding, compression, crypto, parser
///   Layer 3 (leaves): base-types, alloc-utils, string-utils, hash-utils,
///                     io-utils, math-utils, time-utils
fn build_medium_registry() -> MockRegistry {
    let mut registry = MockRegistry::new();

    // Layer 3 -- leaves (7 packages, 2-3 versions each)
    for name in &[
        "base-types",
        "alloc-utils",
        "string-utils",
        "hash-utils",
        "io-utils",
        "math-utils",
        "time-utils",
    ] {
        registry
            .add_version(name, "1.0.0", vec![])
            .add_version(name, "1.1.0", vec![])
            .add_version(name, "1.2.0", vec![]);
    }

    // Layer 2 -- mid-level (5 packages)
    registry
        .add_version(
            "connection-pool",
            "1.0.0",
            vec![("io-utils", ">=1.0"), ("time-utils", ">=1.0")],
        )
        .add_version(
            "connection-pool",
            "1.1.0",
            vec![("io-utils", ">=1.1"), ("time-utils", ">=1.0")],
        );

    registry
        .add_version(
            "encoding",
            "2.0.0",
            vec![("string-utils", ">=1.0"), ("base-types", ">=1.0")],
        )
        .add_version(
            "encoding",
            "2.1.0",
            vec![("string-utils", ">=1.1"), ("base-types", ">=1.0")],
        );

    registry
        .add_version(
            "compression",
            "1.0.0",
            vec![("io-utils", ">=1.0"), ("alloc-utils", ">=1.0")],
        )
        .add_version(
            "compression",
            "1.1.0",
            vec![("io-utils", ">=1.0"), ("alloc-utils", ">=1.1")],
        );

    registry
        .add_version(
            "crypto",
            "3.0.0",
            vec![("hash-utils", ">=1.0"), ("math-utils", ">=1.0")],
        )
        .add_version(
            "crypto",
            "3.1.0",
            vec![("hash-utils", ">=1.1"), ("math-utils", ">=1.0")],
        );

    registry
        .add_version(
            "parser",
            "1.0.0",
            vec![("string-utils", ">=1.0"), ("base-types", ">=1.0")],
        )
        .add_version(
            "parser",
            "1.1.0",
            vec![("string-utils", ">=1.0"), ("base-types", ">=1.1")],
        );

    // Layer 1 -- higher-level (5 packages)
    registry
        .add_version(
            "framework",
            "2.0.0",
            vec![
                ("parser", ">=1.0"),
                ("encoding", ">=2.0"),
                ("crypto", ">=3.0"),
            ],
        )
        .add_version(
            "framework",
            "2.1.0",
            vec![
                ("parser", ">=1.1"),
                ("encoding", ">=2.1"),
                ("crypto", ">=3.0"),
            ],
        );

    registry
        .add_version(
            "http-client",
            "1.0.0",
            vec![
                ("connection-pool", ">=1.0"),
                ("encoding", ">=2.0"),
                ("compression", ">=1.0"),
            ],
        )
        .add_version(
            "http-client",
            "1.1.0",
            vec![
                ("connection-pool", ">=1.1"),
                ("encoding", ">=2.0"),
                ("compression", ">=1.0"),
            ],
        );

    registry
        .add_version(
            "serializer",
            "1.0.0",
            vec![("parser", ">=1.0"), ("encoding", ">=2.0")],
        )
        .add_version(
            "serializer",
            "1.1.0",
            vec![("parser", ">=1.0"), ("encoding", ">=2.1")],
        );

    registry
        .add_version(
            "validator",
            "1.0.0",
            vec![("parser", ">=1.0"), ("crypto", ">=3.0")],
        )
        .add_version(
            "validator",
            "1.1.0",
            vec![("parser", ">=1.1"), ("crypto", ">=3.1")],
        );

    registry.add_version(
        "cache",
        "1.0.0",
        vec![
            ("connection-pool", ">=1.0"),
            ("compression", ">=1.0"),
            ("time-utils", ">=1.0"),
        ],
    );

    // Layer 0 -- root-level packages (3 packages)
    registry.add_version(
        "app-core",
        "1.0.0",
        vec![
            ("framework", ">=2.0"),
            ("serializer", ">=1.0"),
            ("validator", ">=1.0"),
        ],
    );

    registry.add_version(
        "app-web",
        "1.0.0",
        vec![
            ("framework", ">=2.0"),
            ("http-client", ">=1.0"),
            ("cache", ">=1.0"),
        ],
    );

    registry.add_version(
        "app-api",
        "1.0.0",
        vec![
            ("http-client", ">=1.0"),
            ("serializer", ">=1.0"),
            ("validator", ">=1.0"),
        ],
    );

    registry
}

fn bench_resolve_small(c: &mut Criterion) {
    let registry = build_small_registry();
    let config = make_config();
    let requirements = vec![
        Requirement::parse("web-framework>=1.0").unwrap(),
        Requirement::parse("db-driver>=2.0").unwrap(),
    ];

    c.bench_function("resolve_small_5_packages", |b| {
        b.iter(|| {
            let result = resolve(
                black_box(registry.clone()),
                black_box(config.clone()),
                black_box(requirements.clone()),
            );
            black_box(result).unwrap();
        })
    });
}

fn bench_resolve_medium(c: &mut Criterion) {
    let registry = build_medium_registry();
    let config = make_config();
    let requirements = vec![
        Requirement::parse("app-core>=1.0").unwrap(),
        Requirement::parse("app-web>=1.0").unwrap(),
        Requirement::parse("app-api>=1.0").unwrap(),
    ];

    c.bench_function("resolve_medium_20_packages", |b| {
        b.iter(|| {
            let result = resolve(
                black_box(registry.clone()),
                black_box(config.clone()),
                black_box(requirements.clone()),
            );
            black_box(result).unwrap();
        })
    });
}

criterion_group!(benches, bench_resolve_small, bench_resolve_medium);
criterion_main!(benches);
