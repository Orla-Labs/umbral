use criterion::{black_box, criterion_group, criterion_main, Criterion};
use umbral_lockfile::{Artifact, Dependency, LockOptions, LockedPackage, PackageSource, UvLock};

/// A realistic uv.lock string with 12 packages.
fn sample_lockfile() -> String {
    r#"version = 1
revision = 1
requires-python = ">=3.10"

[options]
exclude-newer = "2024-01-01T00:00:00Z"

[[package]]
name = "my-project"
version = "0.1.0"
source = { virtual = "." }
dependencies = [
    { name = "requests", version = "2.31.0" },
    { name = "click", version = "8.1.7" },
    { name = "flask", version = "3.0.0" },
]

[[package]]
name = "requests"
version = "2.31.0"
source = { registry = "https://pypi.org/simple" }
dependencies = [
    { name = "charset-normalizer", version = "3.3.2" },
    { name = "idna", version = "3.6" },
    { name = "urllib3", version = "2.1.0" },
    { name = "certifi", version = "2023.11.17" },
]
sdist = { url = "https://files.pythonhosted.org/packages/requests-2.31.0.tar.gz", hash = "sha256:942c5a758f98d790eaed1a29cb6eefc7f0edf3fcb0fce8aea3fbd5951d9bded8" }
wheels = [
    { url = "https://files.pythonhosted.org/packages/requests-2.31.0-py3-none-any.whl", hash = "sha256:58cd2187c01e70e6e26505bca751777aa9f2ee0b7f4300988b709f44e013003eb" },
]

[[package]]
name = "charset-normalizer"
version = "3.3.2"
source = { registry = "https://pypi.org/simple" }
sdist = { url = "https://files.pythonhosted.org/packages/charset-normalizer-3.3.2.tar.gz", hash = "sha256:f30c3cb33b24454a82faecaf01b19c18562b1e89558fb6c56de4d9118a032fd5" }
wheels = [
    { url = "https://files.pythonhosted.org/packages/charset_normalizer-3.3.2-py3-none-any.whl", hash = "sha256:3e4d1f6587322d2788836a99c69062fbb091331ec940e02d12d179c1d53e25fc" },
]

[[package]]
name = "idna"
version = "3.6"
source = { registry = "https://pypi.org/simple" }
sdist = { url = "https://files.pythonhosted.org/packages/idna-3.6.tar.gz", hash = "sha256:9ecdbbd083b06798ae1e86adcbfe8ab1479cf864e4ee30fe4e46a003d12491ca" }
wheels = [
    { url = "https://files.pythonhosted.org/packages/idna-3.6-py3-none-any.whl", hash = "sha256:c05567e9c24a6b9faaa835c4821bad0590fbb9d5779e7caa6e1cc4978e7eb24f" },
]

[[package]]
name = "urllib3"
version = "2.1.0"
source = { registry = "https://pypi.org/simple" }
sdist = { url = "https://files.pythonhosted.org/packages/urllib3-2.1.0.tar.gz", hash = "sha256:df7aa8afb0148fa78488e7899b2c59b5f4ffcfa82e6c54ccb9dd37c1d7b52d54" }
wheels = [
    { url = "https://files.pythonhosted.org/packages/urllib3-2.1.0-py3-none-any.whl", hash = "sha256:55901e917a5896a349ff771be919f8bd99aff50b79fe58fec595eb37bbc56bb3" },
]

[[package]]
name = "certifi"
version = "2023.11.17"
source = { registry = "https://pypi.org/simple" }
sdist = { url = "https://files.pythonhosted.org/packages/certifi-2023.11.17.tar.gz", hash = "sha256:9b469a0b7d4f6c1f3ab94a37e0a2c4a8e7b0e5c7a0e3b0f5e5e5c7f5a5b5c5d" }
wheels = [
    { url = "https://files.pythonhosted.org/packages/certifi-2023.11.17-py3-none-any.whl", hash = "sha256:a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d2e3f4a5b6c7d8e9f0a1b" },
]

[[package]]
name = "click"
version = "8.1.7"
source = { registry = "https://pypi.org/simple" }
sdist = { url = "https://files.pythonhosted.org/packages/click-8.1.7.tar.gz", hash = "sha256:ca9853ad459e787e2192211578cc907e7594e294c7ccc834310722b41b9ca6de" }
wheels = [
    { url = "https://files.pythonhosted.org/packages/click-8.1.7-py3-none-any.whl", hash = "sha256:ae74fb96c20a0277a1d615f1e4d73c8414f5a98db8b799a7931d1582f3390c28" },
]

[[package]]
name = "flask"
version = "3.0.0"
source = { registry = "https://pypi.org/simple" }
dependencies = [
    { name = "werkzeug", version = "3.0.1" },
    { name = "jinja2", version = "3.1.2" },
    { name = "click", version = "8.1.7" },
]
sdist = { url = "https://files.pythonhosted.org/packages/flask-3.0.0.tar.gz", hash = "sha256:cfadcdb638b609361d29ec22360d6070a77d7b2b0e8d3f08f49b8018c6b0f150" }
wheels = [
    { url = "https://files.pythonhosted.org/packages/flask-3.0.0-py3-none-any.whl", hash = "sha256:21128f47e4e3b9d597a3e8521a329bf56909b690fcc3fa3e477725d5ed857e50" },
]

[[package]]
name = "werkzeug"
version = "3.0.1"
source = { registry = "https://pypi.org/simple" }
sdist = { url = "https://files.pythonhosted.org/packages/werkzeug-3.0.1.tar.gz", hash = "sha256:507e811ecea72b18a404947ead4940c3688b2c3f3f8e7ac4e0329a0e5a7b5e85" }
wheels = [
    { url = "https://files.pythonhosted.org/packages/werkzeug-3.0.1-py3-none-any.whl", hash = "sha256:90a285dc0e42ad56b7d0f7d1a03ac21e7a45f3e807cf6d1ee3a5c98175e6d8b1" },
]

[[package]]
name = "jinja2"
version = "3.1.2"
source = { registry = "https://pypi.org/simple" }
dependencies = [
    { name = "markupsafe", version = "2.1.3" },
]
sdist = { url = "https://files.pythonhosted.org/packages/jinja2-3.1.2.tar.gz", hash = "sha256:31351a702a408a9e7595a8fc6150fc3f43bb6bf7e319770cbc0db9253cebf8cc" }
wheels = [
    { url = "https://files.pythonhosted.org/packages/jinja2-3.1.2-py3-none-any.whl", hash = "sha256:6088930bfe239f0e6710546ab9c19c9ef35e29792895fed6e6e31a023a182a61" },
]

[[package]]
name = "markupsafe"
version = "2.1.3"
source = { registry = "https://pypi.org/simple" }
sdist = { url = "https://files.pythonhosted.org/packages/markupsafe-2.1.3.tar.gz", hash = "sha256:af598ed32d6ae86f1b747b82783958b1a4ab8f617b06fe68795c7f026abbdcad" }
wheels = [
    { url = "https://files.pythonhosted.org/packages/markupsafe-2.1.3-py3-none-any.whl", hash = "sha256:97a68e6ada378df82bc9f16b800ab77cbf4b2fada0081794318520138c088e4a" },
]
"#
    .to_string()
}

/// Build a UvLock struct programmatically for the write benchmark.
fn build_uvlock() -> UvLock {
    let registry_source = PackageSource::Registry {
        url: "https://pypi.org/simple".to_string(),
    };

    let make_dep = |name: &str, version: &str| Dependency {
        name: name.to_string(),
        version: Some(version.to_string()),
        source: None,
        marker: None,
        extra: None,
    };

    let make_pkg = |name: &str, version: &str, deps: Vec<Dependency>, hash: &str| LockedPackage {
        name: name.to_string(),
        version: Some(version.to_string()),
        source: registry_source.clone(),
        dependencies: deps,
        optional_dependencies: std::collections::BTreeMap::new(),
        dev_dependencies: std::collections::BTreeMap::new(),
        sdist: Some(Artifact {
            url: Some(format!(
                "https://files.pythonhosted.org/packages/{}-{}.tar.gz",
                name, version
            )),
            path: None,
            filename: None,
            hash: format!("sha256:{}", hash),
            size: Some(50000),
        }),
        wheels: vec![Artifact {
            url: Some(format!(
                "https://files.pythonhosted.org/packages/{}-{}-py3-none-any.whl",
                name, version
            )),
            path: None,
            filename: None,
            hash: format!("sha256:{}ab", hash),
            size: Some(40000),
        }],
    };

    let h = "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d2e3f4a5b6c7d8e9f0";

    UvLock {
        version: 1,
        revision: 1,
        requires_python: Some(">=3.10".to_string()),
        options: LockOptions::default(),
        packages: vec![
            make_pkg(
                "requests",
                "2.31.0",
                vec![
                    make_dep("charset-normalizer", "3.3.2"),
                    make_dep("idna", "3.6"),
                    make_dep("urllib3", "2.1.0"),
                    make_dep("certifi", "2023.11.17"),
                ],
                h,
            ),
            make_pkg("charset-normalizer", "3.3.2", vec![], h),
            make_pkg("idna", "3.6", vec![], h),
            make_pkg("urllib3", "2.1.0", vec![], h),
            make_pkg("certifi", "2023.11.17", vec![], h),
            make_pkg("click", "8.1.7", vec![], h),
            make_pkg(
                "flask",
                "3.0.0",
                vec![
                    make_dep("werkzeug", "3.0.1"),
                    make_dep("jinja2", "3.1.2"),
                    make_dep("click", "8.1.7"),
                ],
                h,
            ),
            make_pkg("werkzeug", "3.0.1", vec![], h),
            make_pkg("jinja2", "3.1.2", vec![make_dep("markupsafe", "2.1.3")], h),
            make_pkg("markupsafe", "2.1.3", vec![], h),
            make_pkg("numpy", "1.26.3", vec![], h),
            make_pkg("pandas", "2.1.4", vec![make_dep("numpy", "1.26.3")], h),
        ],
    }
}

fn bench_lockfile_parse(c: &mut Criterion) {
    let content = sample_lockfile();

    c.bench_function("parse_lockfile_12_packages", |b| {
        b.iter(|| {
            let lock = UvLock::from_str(black_box(&content)).unwrap();
            black_box(lock);
        })
    });
}

fn bench_lockfile_write(c: &mut Criterion) {
    let lock = build_uvlock();

    c.bench_function("write_lockfile_12_packages", |b| {
        b.iter(|| {
            let output = black_box(&lock).to_toml().unwrap();
            black_box(output);
        })
    });
}

criterion_group!(benches, bench_lockfile_parse, bench_lockfile_write);
criterion_main!(benches);
