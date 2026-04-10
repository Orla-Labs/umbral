//! PEP 508 environment marker evaluation.
//!
//! Provides the [`MarkerTree`] AST, [`MarkerEnvironment`] with all 11
//! marker variables, and evaluation logic including version-aware
//! comparisons for `python_version` / `python_full_version` /
//! `implementation_version`.

use std::str::FromStr;

use serde::{Deserialize, Serialize};
use umbral_pep440::Version;

// ── Marker environment ──────────────────────────────────────────────

/// All 11 PEP 508 environment marker variables.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarkerEnvironment {
    pub os_name: String,
    pub sys_platform: String,
    pub platform_machine: String,
    pub platform_python_implementation: String,
    pub platform_release: String,
    pub platform_system: String,
    pub platform_version: String,
    pub python_version: String,
    pub python_full_version: String,
    pub implementation_name: String,
    pub implementation_version: String,
}

impl MarkerEnvironment {
    /// Look up a marker variable by its PEP 508 name.
    pub fn get(&self, var: &MarkerVariable) -> &str {
        match var {
            MarkerVariable::OsName => &self.os_name,
            MarkerVariable::SysPlatform => &self.sys_platform,
            MarkerVariable::PlatformMachine => &self.platform_machine,
            MarkerVariable::PlatformPythonImplementation => &self.platform_python_implementation,
            MarkerVariable::PlatformRelease => &self.platform_release,
            MarkerVariable::PlatformSystem => &self.platform_system,
            MarkerVariable::PlatformVersion => &self.platform_version,
            MarkerVariable::PythonVersion => &self.python_version,
            MarkerVariable::PythonFullVersion => &self.python_full_version,
            MarkerVariable::ImplementationName => &self.implementation_name,
            MarkerVariable::ImplementationVersion => &self.implementation_version,
            // `extra` is context-dependent, not part of the environment.
            // Return empty string; use `evaluate_with_extras` for proper handling.
            MarkerVariable::Extra => "",
        }
    }

    /// A typical CPython 3.12 on Linux x86_64 environment — useful for tests.
    pub fn cpython_312_linux() -> Self {
        Self {
            os_name: "posix".into(),
            sys_platform: "linux".into(),
            platform_machine: "x86_64".into(),
            platform_python_implementation: "CPython".into(),
            platform_release: "6.5.0-generic".into(),
            platform_system: "Linux".into(),
            platform_version: "#1 SMP PREEMPT_DYNAMIC".into(),
            python_version: "3.12".into(),
            python_full_version: "3.12.3".into(),
            implementation_name: "cpython".into(),
            implementation_version: "3.12.3".into(),
        }
    }

    /// Detect the current host environment's marker values.
    ///
    /// Uses compile-time `cfg` targets for `os_name`, `sys_platform`,
    /// `platform_system`, and `platform_machine`. Python version fields
    /// default to `"3.12"` (matching CPython 3.12); callers should override
    /// these if a specific interpreter is known.
    pub fn current() -> Self {
        let (os_name, sys_platform, platform_system) = if cfg!(target_os = "linux") {
            ("posix", "linux", "Linux")
        } else if cfg!(target_os = "macos") {
            ("posix", "darwin", "Darwin")
        } else if cfg!(target_os = "windows") {
            ("nt", "win32", "Windows")
        } else {
            ("posix", "linux", "Linux") // fallback
        };

        let platform_machine = if cfg!(target_arch = "x86_64") {
            if cfg!(target_os = "windows") {
                "AMD64"
            } else {
                "x86_64"
            }
        } else if cfg!(target_arch = "aarch64") {
            if cfg!(target_os = "macos") {
                "arm64"
            } else {
                "aarch64"
            }
        } else {
            std::env::consts::ARCH
        };

        Self {
            os_name: os_name.into(),
            sys_platform: sys_platform.into(),
            platform_machine: platform_machine.into(),
            platform_python_implementation: "CPython".into(),
            platform_release: "".into(),
            platform_system: platform_system.into(),
            platform_version: "".into(),
            python_version: "3.12".into(),
            python_full_version: "3.12.0".into(),
            implementation_name: "cpython".into(),
            implementation_version: "3.12.0".into(),
        }
    }
}

// ── Marker variables ────────────────────────────────────────────────

/// PEP 508 marker variable names.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarkerVariable {
    OsName,
    SysPlatform,
    PlatformMachine,
    PlatformPythonImplementation,
    PlatformRelease,
    PlatformSystem,
    PlatformVersion,
    PythonVersion,
    PythonFullVersion,
    ImplementationName,
    ImplementationVersion,
    Extra,
}

impl FromStr for MarkerVariable {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "os_name" | "os.name" => Ok(Self::OsName),
            "sys_platform" | "sys.platform" => Ok(Self::SysPlatform),
            "platform_machine" | "platform.machine" => Ok(Self::PlatformMachine),
            "platform_python_implementation" | "platform.python_implementation" => {
                Ok(Self::PlatformPythonImplementation)
            }
            "platform_release" | "platform.release" => Ok(Self::PlatformRelease),
            "platform_system" | "platform.system" => Ok(Self::PlatformSystem),
            "platform_version" | "platform.version" => Ok(Self::PlatformVersion),
            "python_version" => Ok(Self::PythonVersion),
            "python_full_version" => Ok(Self::PythonFullVersion),
            "implementation_name" => Ok(Self::ImplementationName),
            "implementation_version" => Ok(Self::ImplementationVersion),
            "extra" => Ok(Self::Extra),
            _ => Err(format!("unknown marker variable: {s}")),
        }
    }
}

impl MarkerVariable {
    /// Parse a PEP 508 marker variable name string.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        <Self as FromStr>::from_str(s).ok()
    }

    /// Whether this variable should use version-aware comparison.
    pub fn is_version_like(&self) -> bool {
        matches!(
            self,
            Self::PythonVersion | Self::PythonFullVersion | Self::ImplementationVersion
        )
    }
}

// ── Marker operators ────────────────────────────────────────────────

/// PEP 508 marker comparison operators.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MarkerOp {
    Equal,
    NotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    TildeEqual,
    In,
    NotIn,
}

impl FromStr for MarkerOp {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            "==" => Ok(Self::Equal),
            "!=" => Ok(Self::NotEqual),
            "<" => Ok(Self::Less),
            "<=" => Ok(Self::LessEqual),
            ">" => Ok(Self::Greater),
            ">=" => Ok(Self::GreaterEqual),
            "~=" => Ok(Self::TildeEqual),
            "in" => Ok(Self::In),
            "not in" => Ok(Self::NotIn),
            _ => Err(format!("unknown marker operator: {s}")),
        }
    }
}

impl MarkerOp {
    /// Parse an operator string.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        <Self as FromStr>::from_str(s).ok()
    }
}

// ── Marker value ────────────────────────────────────────────────────

/// Left- or right-hand side of a marker expression.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MarkerValue {
    /// A marker variable reference (e.g. `python_version`).
    Variable(MarkerVariable),
    /// A quoted string literal (e.g. `"3.12"`).
    Literal(String),
}

// ── Marker AST ──────────────────────────────────────────────────────

/// AST for marker expressions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MarkerTree {
    Expression(MarkerExpression),
    And(Vec<MarkerTree>),
    Or(Vec<MarkerTree>),
}

impl FromStr for MarkerTree {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        crate::marker::parse_markers(s)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkerExpression {
    pub lhs: MarkerValue,
    pub op: MarkerOp,
    pub rhs: MarkerValue,
}

// ── Evaluation ──────────────────────────────────────────────────────

impl MarkerTree {
    /// Evaluate this marker tree against the given environment.
    ///
    /// Any `extra` comparisons evaluate to `false` (no extras active).
    pub fn evaluate(&self, env: &MarkerEnvironment) -> bool {
        self.evaluate_with_extras(env, &[])
    }

    /// Evaluate this marker tree with a set of active extras.
    ///
    /// An `extra == "foo"` expression is `true` when `"foo"` is in `extras`.
    pub fn evaluate_with_extras(&self, env: &MarkerEnvironment, extras: &[String]) -> bool {
        match self {
            Self::Expression(expr) => expr.evaluate_with_extras(env, extras),
            Self::And(children) => children.iter().all(|c| c.evaluate_with_extras(env, extras)),
            Self::Or(children) => children.iter().any(|c| c.evaluate_with_extras(env, extras)),
        }
    }
}

impl MarkerExpression {
    fn evaluate_with_extras(&self, env: &MarkerEnvironment, extras: &[String]) -> bool {
        // Handle `extra` marker variable specially.
        if self.involves_extra() {
            return self.evaluate_extra(extras);
        }

        let lhs_str = self.resolve_value(&self.lhs, env);
        let rhs_str = self.resolve_value(&self.rhs, env);

        // Determine if we should use version comparison.
        let use_version = self.should_use_version_comparison();

        match &self.op {
            MarkerOp::In => rhs_str.contains(lhs_str.as_str()),
            MarkerOp::NotIn => !rhs_str.contains(lhs_str.as_str()),
            _ if use_version => self.compare_versions(&lhs_str, &rhs_str),
            _ => self.compare_strings(&lhs_str, &rhs_str),
        }
    }

    /// Check if this expression involves the `extra` marker variable.
    fn involves_extra(&self) -> bool {
        matches!(&self.lhs, MarkerValue::Variable(MarkerVariable::Extra))
            || matches!(&self.rhs, MarkerValue::Variable(MarkerVariable::Extra))
    }

    /// Evaluate an `extra == "name"` / `extra != "name"` expression.
    fn evaluate_extra(&self, extras: &[String]) -> bool {
        // Determine the literal value to compare against.
        let literal = match (&self.lhs, &self.rhs) {
            (MarkerValue::Variable(MarkerVariable::Extra), MarkerValue::Literal(s))
            | (MarkerValue::Literal(s), MarkerValue::Variable(MarkerVariable::Extra)) => s,
            _ => return false, // extra compared to another variable -- always false
        };

        // Normalize to lowercase: extras are lowercased at parse time (requirement.rs),
        // but the marker literal may have mixed case (e.g. extra == "Security").
        let normalized_literal = literal.to_lowercase();

        match &self.op {
            MarkerOp::Equal => extras.contains(&normalized_literal),
            MarkerOp::NotEqual => !extras.contains(&normalized_literal),
            MarkerOp::In => {
                let lower_literal = literal.to_lowercase();
                extras
                    .iter()
                    .any(|e| lower_literal.contains(e.to_lowercase().as_str()))
            }
            MarkerOp::NotIn => {
                let lower_literal = literal.to_lowercase();
                !extras
                    .iter()
                    .any(|e| lower_literal.contains(e.to_lowercase().as_str()))
            }
            // Other operators don't make semantic sense for extras; return false.
            _ => false,
        }
    }

    fn resolve_value<'a>(&self, value: &'a MarkerValue, env: &'a MarkerEnvironment) -> String {
        match value {
            MarkerValue::Variable(var) => env.get(var).to_string(),
            MarkerValue::Literal(s) => s.clone(),
        }
    }

    /// Determine if we should use PEP 440 version comparison.
    fn should_use_version_comparison(&self) -> bool {
        let lhs_is_version = matches!(&self.lhs, MarkerValue::Variable(v) if v.is_version_like());
        let rhs_is_version = matches!(&self.rhs, MarkerValue::Variable(v) if v.is_version_like());
        lhs_is_version || rhs_is_version
    }

    fn compare_versions(&self, lhs: &str, rhs: &str) -> bool {
        let lhs_ver = lhs.parse::<Version>();
        let rhs_ver = rhs.parse::<Version>();

        match (lhs_ver, rhs_ver) {
            (Ok(l), Ok(r)) => match &self.op {
                MarkerOp::Equal => l == r,
                MarkerOp::NotEqual => l != r,
                MarkerOp::Less => l < r,
                MarkerOp::LessEqual => l <= r,
                MarkerOp::Greater => l > r,
                MarkerOp::GreaterEqual => l >= r,
                MarkerOp::TildeEqual => tilde_equal(&l, &r),
                // In/NotIn handled before this branch.
                MarkerOp::In | MarkerOp::NotIn => unreachable!(),
            },
            // If either side isn't a valid version, fall back to string comparison.
            _ => self.compare_strings(lhs, rhs),
        }
    }

    fn compare_strings(&self, lhs: &str, rhs: &str) -> bool {
        match &self.op {
            MarkerOp::Equal => lhs == rhs,
            MarkerOp::NotEqual => lhs != rhs,
            MarkerOp::Less => lhs < rhs,
            MarkerOp::LessEqual => lhs <= rhs,
            MarkerOp::Greater => lhs > rhs,
            MarkerOp::GreaterEqual => lhs >= rhs,
            MarkerOp::TildeEqual => lhs == rhs, // no meaningful ~= for strings
            MarkerOp::In => rhs.contains(lhs),
            MarkerOp::NotIn => !rhs.contains(lhs),
        }
    }
}

/// PEP 440 compatible release (`~=`): `lhs ~= rhs` means
/// `lhs >= rhs && lhs == rhs.*` (matching up to the second-to-last
/// release segment of rhs).
fn tilde_equal(lhs: &Version, rhs: &Version) -> bool {
    if lhs.epoch != rhs.epoch {
        return false;
    }
    if rhs.release.len() < 2 {
        return false; // ~= requires at least 2 release segments
    }
    if lhs < rhs {
        return false;
    }
    // The prefix to match is all but the last segment of rhs.release.
    let prefix_len = rhs.release.len() - 1;
    lhs.release[..prefix_len.min(lhs.release.len())]
        == rhs.release[..prefix_len.min(rhs.release.len())]
}

// ── Convenience constructors (for building markers in code / tests) ─

impl MarkerTree {
    /// Build a simple `variable op "literal"` expression.
    pub fn simple(var: MarkerVariable, op: MarkerOp, literal: impl Into<String>) -> Self {
        Self::Expression(MarkerExpression {
            lhs: MarkerValue::Variable(var),
            op,
            rhs: MarkerValue::Literal(literal.into()),
        })
    }
}

// ── Display ─────────────────────────────────────────────────────────

impl std::fmt::Display for MarkerTree {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Expression(expr) => write!(f, "{expr}"),
            Self::And(children) => {
                for (i, child) in children.iter().enumerate() {
                    if i > 0 {
                        write!(f, " and ")?;
                    }
                    // Wrap Or-with-multiple-children in parentheses for precedence.
                    if matches!(child, MarkerTree::Or(parts) if parts.len() > 1) {
                        write!(f, "({child})")?;
                    } else {
                        write!(f, "{child}")?;
                    }
                }
                Ok(())
            }
            Self::Or(children) => {
                for (i, child) in children.iter().enumerate() {
                    if i > 0 {
                        write!(f, " or ")?;
                    }
                    write!(f, "{child}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::fmt::Display for MarkerExpression {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {} {}", self.lhs, self.op, self.rhs)
    }
}

impl std::fmt::Display for MarkerValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Variable(var) => write!(f, "{var}"),
            Self::Literal(s) => write!(f, "\"{s}\""),
        }
    }
}

impl std::fmt::Display for MarkerVariable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::OsName => "os_name",
            Self::SysPlatform => "sys_platform",
            Self::PlatformMachine => "platform_machine",
            Self::PlatformPythonImplementation => "platform_python_implementation",
            Self::PlatformRelease => "platform_release",
            Self::PlatformSystem => "platform_system",
            Self::PlatformVersion => "platform_version",
            Self::PythonVersion => "python_version",
            Self::PythonFullVersion => "python_full_version",
            Self::ImplementationName => "implementation_name",
            Self::ImplementationVersion => "implementation_version",
            Self::Extra => "extra",
        };
        write!(f, "{name}")
    }
}

impl std::fmt::Display for MarkerOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Equal => "==",
            Self::NotEqual => "!=",
            Self::Less => "<",
            Self::LessEqual => "<=",
            Self::Greater => ">",
            Self::GreaterEqual => ">=",
            Self::TildeEqual => "~=",
            Self::In => "in",
            Self::NotIn => "not in",
        };
        write!(f, "{s}")
    }
}

// ── Marker parser ───────────────────────────────────────────────────

/// Parse a marker expression string into a `MarkerTree`.
///
/// Grammar (simplified from PEP 508):
/// ```text
/// marker_or  = marker_and ('or' marker_and)*
/// marker_and = marker_atom ('and' marker_atom)*
/// marker_atom = '(' marker_or ')' | marker_expr
/// marker_expr = marker_value op marker_value
/// marker_value = env_var | quoted_string
/// ```
pub fn parse_markers(input: &str) -> Result<MarkerTree, String> {
    let mut parser = MarkerParser::new(input);
    let tree = parser.parse_or()?;
    parser.skip_ws();
    if parser.pos < parser.input.len() {
        return Err(format!(
            "unexpected trailing content in marker: '{}'",
            &parser.input[parser.pos..]
        ));
    }
    Ok(tree)
}

/// Check whether `s` starts with the keyword `kw` followed by a valid
/// boundary character (space, `(`, `'`, `"`), or is exactly equal to `kw`.
fn starts_with_keyword(s: &str, kw: &str) -> bool {
    if let Some(rest) = s.strip_prefix(kw) {
        rest.is_empty()
            || rest.starts_with(' ')
            || rest.starts_with('(')
            || rest.starts_with('\'')
            || rest.starts_with('"')
    } else {
        false
    }
}

struct MarkerParser<'a> {
    input: &'a str,
    pos: usize,
}

impl<'a> MarkerParser<'a> {
    fn new(input: &'a str) -> Self {
        Self { input, pos: 0 }
    }

    fn skip_ws(&mut self) {
        while self.pos < self.input.len() && self.input.as_bytes()[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
    }

    fn remaining(&self) -> &'a str {
        &self.input[self.pos..]
    }

    fn peek(&self) -> Option<char> {
        self.input[self.pos..].chars().next()
    }

    /// marker_or = marker_and ('or' marker_and)*
    fn parse_or(&mut self) -> Result<MarkerTree, String> {
        let mut children = vec![self.parse_and()?];
        loop {
            self.skip_ws();
            if starts_with_keyword(self.remaining(), "or") {
                self.pos += 2;
                self.skip_ws();
                children.push(self.parse_and()?);
            } else {
                break;
            }
        }
        if children.len() == 1 {
            Ok(children
                .pop()
                .expect("children is non-empty after initial push"))
        } else {
            Ok(MarkerTree::Or(children))
        }
    }

    /// marker_and = marker_atom ('and' marker_atom)*
    fn parse_and(&mut self) -> Result<MarkerTree, String> {
        let mut children = vec![self.parse_atom()?];
        loop {
            self.skip_ws();
            if starts_with_keyword(self.remaining(), "and") {
                self.pos += 3;
                self.skip_ws();
                children.push(self.parse_atom()?);
            } else {
                break;
            }
        }
        if children.len() == 1 {
            Ok(children
                .pop()
                .expect("children is non-empty after initial push"))
        } else {
            Ok(MarkerTree::And(children))
        }
    }

    /// marker_atom = '(' marker_or ')' | marker_expr
    fn parse_atom(&mut self) -> Result<MarkerTree, String> {
        self.skip_ws();
        if self.peek() == Some('(') {
            self.pos += 1;
            let tree = self.parse_or()?;
            self.skip_ws();
            if self.peek() != Some(')') {
                return Err("unclosed parenthesis in marker".into());
            }
            self.pos += 1;
            Ok(tree)
        } else {
            self.parse_expr()
        }
    }

    /// marker_expr = marker_value op marker_value
    fn parse_expr(&mut self) -> Result<MarkerTree, String> {
        self.skip_ws();
        let lhs = self.parse_value()?;
        self.skip_ws();
        let op = self.parse_op()?;
        self.skip_ws();
        let rhs = self.parse_value()?;
        Ok(MarkerTree::Expression(MarkerExpression { lhs, op, rhs }))
    }

    fn parse_value(&mut self) -> Result<MarkerValue, String> {
        self.skip_ws();
        if self.peek() == Some('"') || self.peek() == Some('\'') {
            let quote = self.peek().expect("peek is Some after matching Some above");
            self.pos += 1;
            let start = self.pos;
            while self.pos < self.input.len() && self.input.as_bytes()[self.pos] as char != quote {
                self.pos += 1;
            }
            if self.pos >= self.input.len() {
                return Err("unterminated string in marker".into());
            }
            let s = self.input[start..self.pos].to_string();
            self.pos += 1; // close quote
            Ok(MarkerValue::Literal(s))
        } else {
            // Must be an environment variable name
            let start = self.pos;
            while self.pos < self.input.len() {
                let c = self.input.as_bytes()[self.pos];
                if c.is_ascii_alphanumeric() || c == b'_' || c == b'.' {
                    self.pos += 1;
                } else {
                    break;
                }
            }
            if self.pos == start {
                return Err(format!(
                    "expected marker variable or string at: '{}'",
                    self.remaining()
                ));
            }
            let name = &self.input[start..self.pos];
            match MarkerVariable::from_str(name) {
                Some(var) => Ok(MarkerValue::Variable(var)),
                None => Err(format!("unknown marker variable: '{name}'")),
            }
        }
    }

    fn parse_op(&mut self) -> Result<MarkerOp, String> {
        self.skip_ws();
        let rest = self.remaining();

        // Handle "not in" with flexible whitespace: "not" + any whitespace + "in"
        if let Some(stripped) = rest.strip_prefix("not") {
            let after_not = stripped.trim_start();
            if after_not.starts_with("in") {
                // Compute total bytes consumed: "not" + whitespace + "in"
                let total_len = rest.len() - after_not.len() + 2;
                self.pos += total_len;
                self.skip_ws();
                return Ok(MarkerOp::NotIn);
            }
        }

        // Try other operators
        for (prefix, op) in &[
            ("in", MarkerOp::In),
            ("~=", MarkerOp::TildeEqual),
            ("===", MarkerOp::Equal), // treat === as ==
            ("==", MarkerOp::Equal),
            ("!=", MarkerOp::NotEqual),
            ("<=", MarkerOp::LessEqual),
            (">=", MarkerOp::GreaterEqual),
            ("<", MarkerOp::Less),
            (">", MarkerOp::Greater),
        ] {
            if rest.starts_with(prefix) {
                // For "in", ensure it's followed by whitespace or end
                if *prefix == "in" && rest.len() > 2 && rest.as_bytes()[2].is_ascii_alphanumeric() {
                    continue;
                }
                self.pos += prefix.len();
                return Ok(op.clone());
            }
        }
        Err(format!(
            "expected operator at: '{}'",
            &rest[..rest.len().min(20)]
        ))
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> MarkerEnvironment {
        MarkerEnvironment::cpython_312_linux()
    }

    // ── Basic equality ──────────────────────────────────────────

    #[test]
    fn os_name_equal() {
        let m = MarkerTree::simple(MarkerVariable::OsName, MarkerOp::Equal, "posix");
        assert!(m.evaluate(&env()));
    }

    #[test]
    fn os_name_not_equal() {
        let m = MarkerTree::simple(MarkerVariable::OsName, MarkerOp::Equal, "nt");
        assert!(!m.evaluate(&env()));
    }

    #[test]
    fn sys_platform_equal() {
        let m = MarkerTree::simple(MarkerVariable::SysPlatform, MarkerOp::Equal, "linux");
        assert!(m.evaluate(&env()));
    }

    #[test]
    fn not_equal_operator() {
        let m = MarkerTree::simple(MarkerVariable::OsName, MarkerOp::NotEqual, "nt");
        assert!(m.evaluate(&env()));
    }

    // ── Version comparisons ─────────────────────────────────────

    #[test]
    fn python_version_ge() {
        let m = MarkerTree::simple(
            MarkerVariable::PythonVersion,
            MarkerOp::GreaterEqual,
            "3.10",
        );
        assert!(m.evaluate(&env()));
    }

    #[test]
    fn python_version_lt() {
        let m = MarkerTree::simple(MarkerVariable::PythonVersion, MarkerOp::Less, "3.13");
        assert!(m.evaluate(&env()));
    }

    #[test]
    fn python_version_lt_false() {
        let m = MarkerTree::simple(MarkerVariable::PythonVersion, MarkerOp::Less, "3.10");
        assert!(!m.evaluate(&env()));
    }

    #[test]
    fn python_full_version_ge() {
        let m = MarkerTree::simple(
            MarkerVariable::PythonFullVersion,
            MarkerOp::GreaterEqual,
            "3.12.0",
        );
        assert!(m.evaluate(&env()));
    }

    #[test]
    fn python_full_version_equal() {
        let m = MarkerTree::simple(MarkerVariable::PythonFullVersion, MarkerOp::Equal, "3.12.3");
        assert!(m.evaluate(&env()));
    }

    #[test]
    fn python_full_version_not_equal() {
        let m = MarkerTree::simple(
            MarkerVariable::PythonFullVersion,
            MarkerOp::NotEqual,
            "3.11.0",
        );
        assert!(m.evaluate(&env()));
    }

    #[test]
    fn implementation_version_le() {
        let m = MarkerTree::simple(
            MarkerVariable::ImplementationVersion,
            MarkerOp::LessEqual,
            "3.12.3",
        );
        assert!(m.evaluate(&env()));
    }

    #[test]
    fn implementation_version_gt_false() {
        let m = MarkerTree::simple(
            MarkerVariable::ImplementationVersion,
            MarkerOp::Greater,
            "3.12.3",
        );
        assert!(!m.evaluate(&env()));
    }

    // ── Tilde-equal (~=) ────────────────────────────────────────

    #[test]
    fn tilde_equal_match() {
        // 3.12.3 ~= 3.12.0  →  3.12.3 >= 3.12.0 && same 3.12 prefix
        let m = MarkerTree::simple(
            MarkerVariable::PythonFullVersion,
            MarkerOp::TildeEqual,
            "3.12.0",
        );
        assert!(m.evaluate(&env()));
    }

    #[test]
    fn tilde_equal_no_match() {
        // 3.12.3 ~= 3.11.0  →  3.12.3 >= 3.11.0 but prefix 3.12 != 3.11
        let m = MarkerTree::simple(
            MarkerVariable::PythonFullVersion,
            MarkerOp::TildeEqual,
            "3.11.0",
        );
        assert!(!m.evaluate(&env()));
    }

    // ── In / Not In ─────────────────────────────────────────────

    #[test]
    fn in_operator() {
        let m = MarkerTree::simple(MarkerVariable::SysPlatform, MarkerOp::In, "linux darwin");
        assert!(m.evaluate(&env()));
    }

    #[test]
    fn in_operator_no_match() {
        let m = MarkerTree::simple(MarkerVariable::SysPlatform, MarkerOp::In, "win32 darwin");
        assert!(!m.evaluate(&env()));
    }

    #[test]
    fn not_in_operator() {
        let m = MarkerTree::simple(MarkerVariable::SysPlatform, MarkerOp::NotIn, "win32 darwin");
        assert!(m.evaluate(&env()));
    }

    #[test]
    fn not_in_operator_no_match() {
        let m = MarkerTree::simple(MarkerVariable::SysPlatform, MarkerOp::NotIn, "linux darwin");
        assert!(!m.evaluate(&env()));
    }

    // ── String comparison (lexicographic) ───────────────────────

    #[test]
    fn string_less() {
        let m = MarkerTree::simple(MarkerVariable::OsName, MarkerOp::Less, "z");
        assert!(m.evaluate(&env())); // "posix" < "z"
    }

    #[test]
    fn string_greater_equal() {
        let m = MarkerTree::simple(MarkerVariable::OsName, MarkerOp::GreaterEqual, "posix");
        assert!(m.evaluate(&env()));
    }

    // ── And / Or combinators ────────────────────────────────────

    #[test]
    fn and_both_true() {
        let m = MarkerTree::And(vec![
            MarkerTree::simple(MarkerVariable::OsName, MarkerOp::Equal, "posix"),
            MarkerTree::simple(MarkerVariable::SysPlatform, MarkerOp::Equal, "linux"),
        ]);
        assert!(m.evaluate(&env()));
    }

    #[test]
    fn and_one_false() {
        let m = MarkerTree::And(vec![
            MarkerTree::simple(MarkerVariable::OsName, MarkerOp::Equal, "posix"),
            MarkerTree::simple(MarkerVariable::SysPlatform, MarkerOp::Equal, "win32"),
        ]);
        assert!(!m.evaluate(&env()));
    }

    #[test]
    fn or_one_true() {
        let m = MarkerTree::Or(vec![
            MarkerTree::simple(MarkerVariable::SysPlatform, MarkerOp::Equal, "win32"),
            MarkerTree::simple(MarkerVariable::SysPlatform, MarkerOp::Equal, "linux"),
        ]);
        assert!(m.evaluate(&env()));
    }

    #[test]
    fn or_both_false() {
        let m = MarkerTree::Or(vec![
            MarkerTree::simple(MarkerVariable::SysPlatform, MarkerOp::Equal, "win32"),
            MarkerTree::simple(MarkerVariable::SysPlatform, MarkerOp::Equal, "darwin"),
        ]);
        assert!(!m.evaluate(&env()));
    }

    #[test]
    fn nested_and_or() {
        // (os_name == "posix" and python_version >= "3.10") or sys_platform == "win32"
        let m = MarkerTree::Or(vec![
            MarkerTree::And(vec![
                MarkerTree::simple(MarkerVariable::OsName, MarkerOp::Equal, "posix"),
                MarkerTree::simple(
                    MarkerVariable::PythonVersion,
                    MarkerOp::GreaterEqual,
                    "3.10",
                ),
            ]),
            MarkerTree::simple(MarkerVariable::SysPlatform, MarkerOp::Equal, "win32"),
        ]);
        assert!(m.evaluate(&env()));
    }

    // ── All 11 marker variables resolve correctly ───────────────

    #[test]
    fn all_variables_resolve() {
        let e = env();
        assert_eq!(e.get(&MarkerVariable::OsName), "posix");
        assert_eq!(e.get(&MarkerVariable::SysPlatform), "linux");
        assert_eq!(e.get(&MarkerVariable::PlatformMachine), "x86_64");
        assert_eq!(
            e.get(&MarkerVariable::PlatformPythonImplementation),
            "CPython"
        );
        assert_eq!(e.get(&MarkerVariable::PlatformRelease), "6.5.0-generic");
        assert_eq!(e.get(&MarkerVariable::PlatformSystem), "Linux");
        assert_eq!(
            e.get(&MarkerVariable::PlatformVersion),
            "#1 SMP PREEMPT_DYNAMIC"
        );
        assert_eq!(e.get(&MarkerVariable::PythonVersion), "3.12");
        assert_eq!(e.get(&MarkerVariable::PythonFullVersion), "3.12.3");
        assert_eq!(e.get(&MarkerVariable::ImplementationName), "cpython");
        assert_eq!(e.get(&MarkerVariable::ImplementationVersion), "3.12.3");
    }

    // ── MarkerVariable::from_str ────────────────────────────────

    #[test]
    fn variable_from_str() {
        assert_eq!(
            MarkerVariable::from_str("os_name"),
            Some(MarkerVariable::OsName)
        );
        assert_eq!(
            MarkerVariable::from_str("python_version"),
            Some(MarkerVariable::PythonVersion)
        );
        assert_eq!(MarkerVariable::from_str("bogus"), None);
    }

    #[test]
    fn variable_legacy_names() {
        // PEP 508 also allows os.name, sys.platform, etc.
        assert_eq!(
            MarkerVariable::from_str("os.name"),
            Some(MarkerVariable::OsName)
        );
        assert_eq!(
            MarkerVariable::from_str("sys.platform"),
            Some(MarkerVariable::SysPlatform)
        );
        assert_eq!(
            MarkerVariable::from_str("platform.machine"),
            Some(MarkerVariable::PlatformMachine)
        );
    }

    // ── MarkerOp::from_str ──────────────────────────────────────

    #[test]
    fn op_from_str() {
        assert_eq!(MarkerOp::from_str("=="), Some(MarkerOp::Equal));
        assert_eq!(MarkerOp::from_str("!="), Some(MarkerOp::NotEqual));
        assert_eq!(MarkerOp::from_str("<"), Some(MarkerOp::Less));
        assert_eq!(MarkerOp::from_str("<="), Some(MarkerOp::LessEqual));
        assert_eq!(MarkerOp::from_str(">"), Some(MarkerOp::Greater));
        assert_eq!(MarkerOp::from_str(">="), Some(MarkerOp::GreaterEqual));
        assert_eq!(MarkerOp::from_str("~="), Some(MarkerOp::TildeEqual));
        assert_eq!(MarkerOp::from_str("in"), Some(MarkerOp::In));
        assert_eq!(MarkerOp::from_str("not in"), Some(MarkerOp::NotIn));
        assert_eq!(MarkerOp::from_str("??"), None);
    }

    // ── Edge cases ──────────────────────────────────────────────

    #[test]
    fn empty_and_is_true() {
        let m = MarkerTree::And(vec![]);
        assert!(m.evaluate(&env())); // vacuous truth
    }

    #[test]
    fn empty_or_is_false() {
        let m = MarkerTree::Or(vec![]);
        assert!(!m.evaluate(&env()));
    }

    #[test]
    fn version_fallback_to_string_on_invalid_version() {
        // If rhs isn't a valid PEP 440 version, fall back to string comparison.
        let m = MarkerTree::simple(
            MarkerVariable::PythonVersion,
            MarkerOp::NotEqual,
            "not-a-version",
        );
        assert!(m.evaluate(&env()));
    }

    // ── Platform-specific markers ───────────────────────────────

    #[test]
    fn platform_python_implementation() {
        let m = MarkerTree::simple(
            MarkerVariable::PlatformPythonImplementation,
            MarkerOp::Equal,
            "CPython",
        );
        assert!(m.evaluate(&env()));
    }

    #[test]
    fn implementation_name() {
        let m = MarkerTree::simple(
            MarkerVariable::ImplementationName,
            MarkerOp::Equal,
            "cpython",
        );
        assert!(m.evaluate(&env()));
    }

    #[test]
    fn platform_system() {
        let m = MarkerTree::simple(MarkerVariable::PlatformSystem, MarkerOp::Equal, "Linux");
        assert!(m.evaluate(&env()));
    }

    // ── Extra marker variable (Fix 1) ────────────────────────────

    #[test]
    fn extra_variable_from_str() {
        assert_eq!(
            MarkerVariable::from_str("extra"),
            Some(MarkerVariable::Extra)
        );
    }

    #[test]
    fn extra_parsed_from_marker_string() {
        let tree = parse_markers("extra == \"security\"").unwrap();
        match &tree {
            MarkerTree::Expression(expr) => {
                assert_eq!(expr.lhs, MarkerValue::Variable(MarkerVariable::Extra));
                assert_eq!(expr.rhs, MarkerValue::Literal("security".into()));
            }
            other => panic!("expected Expression, got: {other:?}"),
        }
    }

    #[test]
    fn extra_evaluate_no_extras_is_false() {
        let m = MarkerTree::simple(MarkerVariable::Extra, MarkerOp::Equal, "security");
        // No extras active → false
        assert!(!m.evaluate(&env()));
    }

    #[test]
    fn extra_evaluate_with_matching_extra() {
        let m = MarkerTree::simple(MarkerVariable::Extra, MarkerOp::Equal, "security");
        let extras = vec!["security".to_string()];
        assert!(m.evaluate_with_extras(&env(), &extras));
    }

    #[test]
    fn extra_evaluate_with_non_matching_extra() {
        let m = MarkerTree::simple(MarkerVariable::Extra, MarkerOp::Equal, "security");
        let extras = vec!["dev".to_string()];
        assert!(!m.evaluate_with_extras(&env(), &extras));
    }

    #[test]
    fn extra_not_equal_with_matching_extra() {
        let m = MarkerTree::simple(MarkerVariable::Extra, MarkerOp::NotEqual, "security");
        let extras = vec!["security".to_string()];
        assert!(!m.evaluate_with_extras(&env(), &extras));
    }

    #[test]
    fn extra_not_equal_without_matching_extra() {
        let m = MarkerTree::simple(MarkerVariable::Extra, MarkerOp::NotEqual, "security");
        let extras = vec!["dev".to_string()];
        assert!(m.evaluate_with_extras(&env(), &extras));
    }

    #[test]
    fn extra_display() {
        let m = MarkerTree::simple(MarkerVariable::Extra, MarkerOp::Equal, "security");
        assert_eq!(m.to_string(), "extra == \"security\"");
    }

    #[test]
    fn extra_combined_with_other_markers() {
        // python_version >= "3.8" and extra == "security"
        let tree = parse_markers("python_version >= \"3.8\" and extra == \"security\"").unwrap();
        let extras = vec!["security".to_string()];
        assert!(tree.evaluate_with_extras(&env(), &extras));
        // Without extras → false because of the 'and'
        assert!(!tree.evaluate(&env()));
    }

    // ── Fix 2: extra in/not in containment direction ────────────

    #[test]
    fn extra_in_literal_string_match() {
        // extra in "security socks" with active extra "security" → true
        let m = MarkerTree::simple(MarkerVariable::Extra, MarkerOp::In, "security socks");
        let extras = vec!["security".to_string()];
        assert!(
            m.evaluate_with_extras(&env(), &extras),
            "extra in \"security socks\" should be true when 'security' is active"
        );
    }

    #[test]
    fn extra_in_literal_string_no_match() {
        // extra in "security socks" with active extra "other" → false
        let m = MarkerTree::simple(MarkerVariable::Extra, MarkerOp::In, "security socks");
        let extras = vec!["other".to_string()];
        assert!(
            !m.evaluate_with_extras(&env(), &extras),
            "extra in \"security socks\" should be false when 'other' is active"
        );
    }

    #[test]
    fn extra_not_in_literal_string_match() {
        // extra not in "security socks" with active extra "other" → true
        let m = MarkerTree::simple(MarkerVariable::Extra, MarkerOp::NotIn, "security socks");
        let extras = vec!["other".to_string()];
        assert!(
            m.evaluate_with_extras(&env(), &extras),
            "extra not in \"security socks\" should be true when 'other' is active"
        );
    }

    #[test]
    fn extra_not_in_literal_string_no_match() {
        // extra not in "security socks" with active extra "security" → false
        let m = MarkerTree::simple(MarkerVariable::Extra, MarkerOp::NotIn, "security socks");
        let extras = vec!["security".to_string()];
        assert!(
            !m.evaluate_with_extras(&env(), &extras),
            "extra not in \"security socks\" should be false when 'security' is active"
        );
    }

    // ── Fix: tilde_equal epoch mismatch ────────────────────────────

    #[test]
    fn tilde_equal_epoch_mismatch_returns_false() {
        // python_full_version ~= "1!3.12.0" should NOT match epoch-0 version 3.12.3
        let m = MarkerTree::simple(
            MarkerVariable::PythonFullVersion,
            MarkerOp::TildeEqual,
            "1!3.12.0",
        );
        assert!(
            !m.evaluate(&env()),
            "~= with epoch 1 should not match epoch 0 version 3.12.3"
        );
    }

    #[test]
    fn tilde_equal_single_segment_returns_false() {
        // ~= with a single release segment (e.g. "3") is invalid per PEP 440
        // and should never match.
        let lhs: Version = "3.12.3".parse().unwrap();
        let rhs: Version = "3".parse().unwrap();
        assert!(
            !tilde_equal(&lhs, &rhs),
            "tilde_equal with single-segment rhs should return false"
        );
    }

    // ── Fix 1: case-insensitive extra comparison ─────────────────

    #[test]
    fn extra_case_insensitive_security() {
        // Marker says "Security" (mixed case), active extra is "security" (lowercased at parse time)
        let m = MarkerTree::simple(MarkerVariable::Extra, MarkerOp::Equal, "Security");
        let extras = vec!["security".to_string()];
        assert!(
            m.evaluate_with_extras(&env(), &extras),
            "extra == \"Security\" should match lowercased 'security'"
        );
    }

    #[test]
    fn extra_case_insensitive_socks() {
        // Marker says "SOCKS" (upper case), active extra is "socks" (lowercased at parse time)
        let m = MarkerTree::simple(MarkerVariable::Extra, MarkerOp::Equal, "SOCKS");
        let extras = vec!["socks".to_string()];
        assert!(
            m.evaluate_with_extras(&env(), &extras),
            "extra == \"SOCKS\" should match lowercased 'socks'"
        );
    }

    // ── Fix 2: or/and keyword followed by parenthesis ────────────

    #[test]
    fn or_followed_by_paren_parses() {
        // python_version >= "3.8" or(extra == "dev")
        let tree = parse_markers("python_version >= \"3.8\" or(extra == \"dev\")").unwrap();
        let extras = vec!["dev".to_string()];
        assert!(tree.evaluate_with_extras(&env(), &extras));
    }

    #[test]
    fn and_followed_by_paren_parses() {
        // os_name == "posix" and(python_version >= "3.8")
        let tree = parse_markers("os_name == \"posix\" and(python_version >= \"3.8\")").unwrap();
        assert!(tree.evaluate(&env()));
    }

    // ── Fix 3: not in with multiple spaces ───────────────────────

    #[test]
    fn not_in_triple_space_parses() {
        // "not   in" with 3 spaces between not and in
        let tree = parse_markers("sys_platform not   in \"win32 darwin\"").unwrap();
        assert!(tree.evaluate(&env()));
    }

    // ── Edge case: Marker with `in` operator on platform ───────

    #[test]
    fn in_operator_matches_linux() {
        let tree = parse_markers("sys_platform in \"linux darwin\"").unwrap();
        assert!(
            tree.evaluate(&env()),
            "sys_platform 'linux' should be 'in' the string 'linux darwin'"
        );
    }

    #[test]
    fn in_operator_matches_darwin() {
        let tree = parse_markers("sys_platform in \"linux darwin\"").unwrap();
        let mut darwin_env = env();
        darwin_env.sys_platform = "darwin".into();
        assert!(
            tree.evaluate(&darwin_env),
            "sys_platform 'darwin' should be 'in' the string 'linux darwin'"
        );
    }

    // ── Edge case: Marker with `not in` operator ───────────────

    #[test]
    fn not_in_operator_excludes_win32_and_cygwin() {
        let tree = parse_markers("sys_platform not in \"win32 cygwin\"").unwrap();
        // Linux environment should pass (not in win32/cygwin)
        assert!(tree.evaluate(&env()));
    }

    #[test]
    fn not_in_operator_fails_for_win32() {
        let tree = parse_markers("sys_platform not in \"win32 cygwin\"").unwrap();
        let mut win_env = env();
        win_env.sys_platform = "win32".into();
        assert!(
            !tree.evaluate(&win_env),
            "sys_platform 'win32' should fail 'not in' check"
        );
    }

    // ── Edge case: Complex nested marker with or/and precedence ─

    #[test]
    fn complex_nested_or_and_precedence() {
        // python_version >= "3.8" and (sys_platform == "linux" or sys_platform == "darwin")
        let tree = parse_markers(
            "python_version >= \"3.8\" and (sys_platform == \"linux\" or sys_platform == \"darwin\")",
        )
        .unwrap();

        // Linux with Python 3.12 -> true
        assert!(tree.evaluate(&env()));

        // Darwin with Python 3.12 -> true
        let mut darwin_env = env();
        darwin_env.sys_platform = "darwin".into();
        assert!(tree.evaluate(&darwin_env));

        // Windows with Python 3.12 -> false (not linux or darwin)
        let mut win_env = env();
        win_env.sys_platform = "win32".into();
        assert!(!tree.evaluate(&win_env));
    }

    // ── Edge case: Marker with `platform_machine` variable ─────

    #[test]
    fn platform_machine_aarch64() {
        let tree = parse_markers("platform_machine == \"aarch64\"").unwrap();
        let mut arm_env = env();
        arm_env.platform_machine = "aarch64".into();
        assert!(tree.evaluate(&arm_env));

        // Default test env is x86_64, should fail
        assert!(!tree.evaluate(&env()));
    }

    #[test]
    fn platform_machine_x86_64() {
        let tree = parse_markers("platform_machine == \"x86_64\"").unwrap();
        // Default test env is x86_64
        assert!(tree.evaluate(&env()));
    }
}
