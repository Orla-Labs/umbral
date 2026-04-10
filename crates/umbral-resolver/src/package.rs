//! Package types for the PubGrub solver.

use std::fmt;
use umbral_pep440::PackageName;

/// The package type used in PubGrub resolution.
///
/// PubGrub operates on a flat namespace of packages. We model Python's
/// packaging concepts (extras, Python version constraints) as virtual packages
/// in this namespace.
#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub enum UmbralPackage {
    /// The root project being resolved.
    Root,
    /// A regular PyPI package.
    Package(PackageName),
    /// A package extra, modeled as a virtual package.
    /// `Extra("requests", "security")` represents `requests[security]`.
    /// It depends on the base package at the exact same version, plus
    /// whatever additional dependencies the extra declares.
    Extra(PackageName, String),
    /// Virtual package representing the Python interpreter version.
    /// Its only available "version" is the target Python version.
    /// Packages with `requires-python` declare a dependency on this,
    /// allowing PubGrub to backtrack on Python-incompatible versions.
    Python,
}

impl fmt::Display for UmbralPackage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UmbralPackage::Root => write!(f, "your project"),
            UmbralPackage::Package(name) => write!(f, "{}", name),
            UmbralPackage::Extra(name, extra) => write!(f, "{}[{}]", name, extra),
            UmbralPackage::Python => write!(f, "Python"),
        }
    }
}
