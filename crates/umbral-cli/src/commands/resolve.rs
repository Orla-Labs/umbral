use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use clap::Parser;
use miette::{Context, IntoDiagnostic, Result};
use owo_colors::OwoColorize;
use tracing::{debug, info, warn};

use super::{detect_python_version, dirs_cache_dir, discover_workspace};

#[derive(Debug, Parser)]
pub struct ResolveArgs {
    /// Path to pyproject.toml
    #[arg(long, default_value = "./pyproject.toml")]
    project: PathBuf,

    /// Python version to resolve for (e.g. "3.12")
    #[arg(long)]
    python_version: Option<String>,

    /// Base package index URL
    #[arg(long, default_value = "https://pypi.org/simple/")]
    index_url: String,

    /// Additional package index URLs
    #[arg(long)]
    extra_index_url: Vec<String>,

    /// Allow pre-release versions
    #[arg(long)]
    pre: bool,

    /// Ignore cached data
    #[arg(long, alias = "refresh")]
    no_cache: bool,

    /// Output lockfile path
    #[arg(short, long, default_value = "./uv.lock")]
    output: PathBuf,

    /// Generate a universal (cross-platform) lockfile for all target platforms
    #[arg(long)]
    pub universal: bool,
}

impl ResolveArgs {
    /// Create a `ResolveArgs` with defaults for programmatic use (e.g. from `add`/`remove`).
    pub fn for_project(project: PathBuf) -> Self {
        Self {
            project,
            python_version: None,
            index_url: "https://pypi.org/simple/".to_string(),
            extra_index_url: Vec::new(),
            pre: false,
            no_cache: false,
            output: PathBuf::from("./uv.lock"),
            universal: false,
        }
    }
}

pub fn cmd_resolve(args: ResolveArgs) -> Result<()> {
    // -- Step 1: Read pyproject.toml --
    let project_path = &args.project;
    info!(path = %project_path.display(), "reading project file");

    let pyproject = umbral_project::PyProject::from_path(project_path)
        .into_diagnostic()
        .wrap_err_with(|| format!("in {}", project_path.display()))?;

    let project_table = pyproject.project.as_ref().ok_or_else(|| {
        miette::miette!("[project] table is missing from {}", project_path.display())
    })?;

    let project_name = &project_table.name;

    // -- Workspace discovery --
    let project_dir = project_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    let workspace = discover_workspace(project_dir);

    let (dep_strings, output_path) = if let Some(ref ws) = workspace {
        eprintln!(
            "{} {} workspace {} ({} member(s))",
            "●".green().bold(),
            "Resolving".bold(),
            ws.root_project
                .project
                .as_ref()
                .map(|p| p.name.as_str())
                .unwrap_or("(unnamed)")
                .cyan(),
            ws.members.len(),
        );
        let deps = ws.all_dependencies();
        // Write lockfile to workspace root.
        let lockfile_path = ws.root.join("uv.lock");
        (deps, lockfile_path)
    } else {
        eprintln!(
            "{} {} project {}",
            "●".green().bold(),
            "Resolving".bold(),
            project_name.cyan()
        );
        (pyproject.all_dependencies(), args.output.clone())
    };

    // -- Step 2: Extract dependencies --
    if dep_strings.is_empty() {
        warn!("no dependencies found in [project.dependencies]");
        eprintln!("{} No dependencies to resolve.", "⚠".yellow().bold());
        return Ok(());
    }

    eprintln!(
        "  {} {} direct dependencies",
        "→".dimmed(),
        dep_strings.len()
    );

    if let Some(requires_python) = project_table.requires_python.as_deref() {
        eprintln!(
            "  {} requires-python: {}",
            "→".dimmed(),
            requires_python.dimmed()
        );
    }

    // Determine target Python version
    let detected_version = detect_python_version();
    let python_version_str = args.python_version.as_deref().unwrap_or(&detected_version);

    eprintln!(
        "  {} target python: {}",
        "→".dimmed(),
        python_version_str.dimmed()
    );

    if !args.extra_index_url.is_empty() {
        eprintln!(
            "  {} {} extra index URL(s)",
            "→".dimmed(),
            args.extra_index_url.len()
        );
    }

    info!(
        index = %args.index_url,
        extra_indexes = ?args.extra_index_url,
        pre = args.pre,
        no_cache = args.no_cache,
        "resolve configuration"
    );

    // Parse PEP 508 requirement strings into typed Requirements.
    let mut requirements: Vec<umbral_pep508::Requirement> = Vec::new();
    let mut parse_errors: Vec<String> = Vec::new();

    for s in &dep_strings {
        match umbral_pep508::Requirement::parse(s) {
            Ok(req) => requirements.push(req),
            Err(e) => {
                parse_errors.push(format!("  {}: {}", s, e));
            }
        }
    }

    if !parse_errors.is_empty() {
        return Err(miette::miette!(
            "failed to parse {} requirement(s):\n{}",
            parse_errors.len(),
            parse_errors.join("\n")
        ));
    }

    // -- Step 3: Resolve against live PyPI --
    let started = Instant::now();

    // Build a tokio runtime for the async PyPI client.
    let rt = tokio::runtime::Runtime::new()
        .into_diagnostic()
        .wrap_err("failed to create async runtime")?;

    let index_url: url::Url = args
        .index_url
        .parse()
        .into_diagnostic()
        .wrap_err("invalid --index-url")?;

    let extra_urls: Vec<url::Url> = args
        .extra_index_url
        .iter()
        .map(|s| {
            s.parse::<url::Url>()
                .into_diagnostic()
                .wrap_err_with(|| format!("invalid --extra-index-url: {}", s))
        })
        .collect::<Result<_>>()?;

    let cache_dir = dirs_cache_dir().join("pypi");

    let client = Arc::new(
        umbral_pypi_client::SimpleApiClient::with_extra_urls(index_url, extra_urls, cache_dir)
            .into_diagnostic()
            .wrap_err("failed to create PyPI client")?,
    );

    let source = umbral_resolver::LivePypiSource::new(Arc::clone(&client), rt.handle().clone());

    let python_version: umbral_pep440::Version = python_version_str
        .parse()
        .into_diagnostic()
        .wrap_err("invalid --python-version")?;

    let pre_release_policy = if args.pre {
        umbral_resolver::PreReleasePolicy::Allow
    } else {
        umbral_resolver::PreReleasePolicy::Disallow
    };

    let config = umbral_resolver::ResolverConfig {
        python_version,
        markers: None,
        pre_release_policy,
    };

    // -- Parse constraint-dependencies and override-dependencies from [tool.uv] --
    let tool_uv = pyproject.tool_uv();

    let constraint_requirements: Vec<umbral_pep508::Requirement> = tool_uv
        .map(|uv| &uv.constraint_dependencies)
        .into_iter()
        .flatten()
        .map(|s| {
            umbral_pep508::Requirement::parse(s)
                .map_err(|e| miette::miette!("invalid constraint-dependency '{}': {}", s, e))
        })
        .collect::<Result<_>>()?;

    let override_map: std::collections::HashMap<
        umbral_pep440::PackageName,
        umbral_pep440::VersionSpecifiers,
    > = tool_uv
        .map(|uv| &uv.override_dependencies)
        .into_iter()
        .flatten()
        .map(|s| {
            let req = umbral_pep508::Requirement::parse(s)
                .map_err(|e| miette::miette!("invalid override-dependency '{}': {}", s, e))?;
            let spec = req.version.ok_or_else(|| {
                miette::miette!(
                    "override-dependency '{}' must include a version specifier",
                    s
                )
            })?;
            Ok((req.name, spec))
        })
        .collect::<Result<_>>()?;

    if !constraint_requirements.is_empty() {
        debug!(
            "{} constraint-dependencies from [tool.uv]",
            constraint_requirements.len()
        );
    }
    if !override_map.is_empty() {
        debug!(
            "{} override-dependencies from [tool.uv]",
            override_map.len()
        );
    }

    eprintln!(
        "\n  {} Fetching package metadata from index...",
        "→".dimmed(),
    );

    if args.universal {
        // -- Universal (cross-platform) resolution --
        eprintln!(
            "  {} Resolving for all target platforms (universal)...",
            "→".dimmed(),
        );

        let universal = umbral_resolver::resolve_universal_with_constraints(
            &source,
            &requirements,
            &config,
            &constraint_requirements,
            &override_map,
        )
        .map_err(|e| miette::miette!("{}", e))?;

        let elapsed = started.elapsed();
        eprintln!(
            "  {} Resolved {} packages universally in {:.1?}",
            "✓".green().bold(),
            universal.packages.len(),
            elapsed
        );

        // Convert UniversalResolution to lockfile format
        let locked_packages: Vec<umbral_lockfile::LockedPackage> = universal
            .packages
            .values()
            .map(|pkg| umbral_lockfile::LockedPackage {
                name: pkg.name.as_str().to_string(),
                version: Some(pkg.version.to_string()),
                source: umbral_lockfile::PackageSource::Registry {
                    url: args.index_url.clone(),
                },
                dependencies: pkg
                    .dependencies
                    .iter()
                    .map(|(name, _specs, marker)| umbral_lockfile::Dependency {
                        name: name.as_str().to_string(),
                        version: None,
                        source: None,
                        marker: marker.clone(),
                        extra: None,
                    })
                    .collect(),
                optional_dependencies: Default::default(),
                dev_dependencies: Default::default(),
                sdist: None,
                wheels: pkg
                    .artifacts
                    .iter()
                    .map(|a| umbral_lockfile::Artifact {
                        url: Some(a.url.clone()),
                        path: None,
                        filename: Some(a.filename.clone()),
                        hash: a
                            .hash
                            .clone()
                            .unwrap_or_else(|| "sha256:unknown".to_string()),
                        size: a.size,
                    })
                    .collect(),
            })
            .collect();

        let mut uv_lock =
            umbral_lockfile::UvLock::from_resolution(locked_packages, Some(python_version_str));

        // Add resolution-markers to options
        uv_lock.options.resolution_markers =
            Some(umbral_resolver::resolution_markers_for_default_environments());

        uv_lock
            .write_to(&output_path)
            .into_diagnostic()
            .wrap_err_with(|| format!("failed to write lockfile to {}", output_path.display()))?;

        eprintln!(
            "\n{} Wrote universal lockfile to {}",
            "✓".green().bold(),
            output_path.display().to_string().cyan()
        );

        // Print resolved packages
        let mut sorted: Vec<_> = universal.packages.values().collect();
        sorted.sort_by(|a, b| a.name.as_str().cmp(b.name.as_str()));
        for pkg in &sorted {
            let marker_info = pkg
                .marker
                .as_deref()
                .map(|m| format!(" ; {}", m))
                .unwrap_or_default();
            debug!("  {} {} {}{}", "·", pkg.name, pkg.version, marker_info);
        }
    } else {
        // -- Single-platform resolution --
        let mut resolution = umbral_resolver::resolve_with_constraints(
            source,
            config,
            requirements,
            constraint_requirements,
            override_map,
            None,
        )
        .map_err(|e| miette::miette!("{}", e))?;

        // Stamp each resolved package with the index URL it came from.
        for pkg in resolution.packages.values_mut() {
            pkg.source_url = Some(args.index_url.clone());
        }

        let elapsed = started.elapsed();
        eprintln!(
            "  {} Resolved {} packages in {:.1?}",
            "✓".green().bold(),
            resolution.packages.len(),
            elapsed
        );

        // -- Step 4: Write lockfile (uv.lock format) --
        let locked_packages: Vec<umbral_lockfile::LockedPackage> = resolution
            .packages
            .values()
            .map(|pkg| {
                let source_url = pkg
                    .source_url
                    .as_deref()
                    .unwrap_or(&args.index_url)
                    .to_string();

                umbral_lockfile::LockedPackage {
                    name: pkg.name.as_str().to_string(),
                    version: Some(pkg.version.to_string()),
                    source: umbral_lockfile::PackageSource::Registry { url: source_url },
                    dependencies: pkg
                        .dependencies
                        .iter()
                        .map(|(name, _specs)| umbral_lockfile::Dependency {
                            name: name.as_str().to_string(),
                            version: None,
                            source: None,
                            marker: None,
                            extra: None,
                        })
                        .collect(),
                    optional_dependencies: Default::default(),
                    dev_dependencies: Default::default(),
                    sdist: None,
                    wheels: pkg
                        .artifacts
                        .iter()
                        .map(|a| umbral_lockfile::Artifact {
                            url: Some(a.url.clone()),
                            path: None,
                            filename: Some(a.filename.clone()),
                            hash: a
                                .hash
                                .clone()
                                .unwrap_or_else(|| "sha256:unknown".to_string()),
                            size: a.size,
                        })
                        .collect(),
                }
            })
            .collect();

        let uv_lock =
            umbral_lockfile::UvLock::from_resolution(locked_packages, Some(python_version_str));

        uv_lock
            .write_to(&output_path)
            .into_diagnostic()
            .wrap_err_with(|| format!("failed to write lockfile to {}", output_path.display()))?;

        eprintln!(
            "\n{} Wrote lockfile to {}",
            "✓".green().bold(),
            output_path.display().to_string().cyan()
        );

        // Print resolved packages
        let mut sorted: Vec<_> = resolution.packages.values().collect();
        sorted.sort_by_key(|p| &p.name);
        for pkg in &sorted {
            debug!("  {} {} {}", "·", pkg.name, pkg.version);
        }
    }

    Ok(())
}
