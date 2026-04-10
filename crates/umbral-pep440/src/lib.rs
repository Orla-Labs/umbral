//! PEP 440 version parsing, normalization, specifiers, and ordering.
//!
//! This crate implements the Python version identification and dependency
//! specification standard (PEP 440) in pure Rust. It provides:
//!
//! - [`Version`]: A parsed, normalized Python version
//! - [`VersionSpecifier`]: A single version constraint (e.g., `>=1.0`)
//! - [`VersionSpecifiers`]: A comma-separated list of specifiers
//! - [`PackageName`]: A PEP 503-normalized package name

mod package_name;
mod specifier;
mod version;

pub use package_name::PackageName;
pub use specifier::{Operator, VersionSpecifier, VersionSpecifiers};
pub use version::{LocalSegment, ParseError, PreRelease, PreReleaseKind, Version};
