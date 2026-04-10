//! PEP 735 dependency-group expansion with cycle detection.

use std::collections::{HashMap, HashSet};

use indexmap::IndexSet;

use crate::{DependencyGroupSpecifier, ProjectError};

/// Expand a dependency group by name, recursively resolving
/// `{include-group = "…"}` entries.
///
/// Returns a flat list of PEP 508 requirement strings.
pub fn expand_dependency_group(
    groups: &HashMap<String, Vec<DependencyGroupSpecifier>>,
    name: &str,
) -> Result<Vec<String>, ProjectError> {
    let mut visited = HashSet::new();
    expand_inner(groups, name, &mut visited)
}

fn expand_inner(
    groups: &HashMap<String, Vec<DependencyGroupSpecifier>>,
    name: &str,
    visited: &mut HashSet<String>,
) -> Result<Vec<String>, ProjectError> {
    if !visited.insert(name.to_string()) {
        return Err(ProjectError::DependencyGroupCycle(name.to_string()));
    }

    let entries = groups
        .get(name)
        .ok_or_else(|| ProjectError::UnknownDependencyGroup(name.to_string()))?;

    let mut result = IndexSet::new();

    for entry in entries {
        match entry {
            DependencyGroupSpecifier::Requirement(req) => {
                result.insert(req.clone());
            }
            DependencyGroupSpecifier::IncludeGroup { include_group } => {
                let expanded = expand_inner(groups, include_group, visited)?;
                for item in expanded {
                    result.insert(item);
                }
            }
        }
    }

    // Remove from visited after processing (backtracking) so that diamond
    // patterns (B->D, C->D) don't falsely trigger cycle detection.
    visited.remove(name);

    Ok(result.into_iter().collect())
}
