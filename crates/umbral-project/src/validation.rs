//! Validation rules for pyproject.toml.

use crate::{ProjectError, PyProject};

/// Validate a parsed `PyProject`.
///
/// Rules enforced:
/// - `name` cannot appear in `dynamic`
/// - A field that is set statically cannot also be listed in `dynamic`
pub fn validate(pyproject: &PyProject) -> Result<(), ProjectError> {
    let Some(project) = &pyproject.project else {
        return Ok(());
    };

    let Some(dynamic) = &project.dynamic else {
        return Ok(());
    };

    // PEP 621: "name" MUST NOT be listed in dynamic.
    if dynamic.iter().any(|d| d == "name") {
        return Err(ProjectError::Validation(
            "\"name\" must not be listed in [project.dynamic]".into(),
        ));
    }

    // A field that is set statically must not also appear in dynamic.
    let static_fields: &[(&str, bool)] = &[
        ("version", project.version.is_some()),
        ("description", project.description.is_some()),
        ("dependencies", project.dependencies.is_some()),
        (
            "optional-dependencies",
            project.optional_dependencies.is_some(),
        ),
        ("requires-python", project.requires_python.is_some()),
        ("scripts", project.scripts.is_some()),
        ("gui-scripts", project.gui_scripts.is_some()),
        ("readme", project.readme.is_some()),
        ("license", project.license.is_some()),
        ("license-files", project.license_files.is_some()),
        ("authors", project.authors.is_some()),
        ("maintainers", project.maintainers.is_some()),
        ("keywords", project.keywords.is_some()),
        ("classifiers", project.classifiers.is_some()),
        ("urls", project.urls.is_some()),
        ("entry-points", project.entry_points.is_some()),
    ];

    for (field, is_static) in static_fields {
        if *is_static && dynamic.iter().any(|d| d == *field) {
            return Err(ProjectError::Validation(format!(
                "\"{field}\" is set statically and also listed in [project.dynamic]"
            )));
        }
    }

    Ok(())
}
