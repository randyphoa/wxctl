//! Deployment identity (SaaS vs IBM Software Hub).
//!
//! Profiles use *concrete* deployments (`saas`, `software-5.3.0`).
//! Schema `deployments:` keys and `metadata.requires.deployment` use *constraints*
//! (`saas`, `software`, `software-5.3.x`, `>=software-5.3, <software-6`).
//!
//! Phase 1: single-constraint form only. The `,`-separated multi-clause grammar
//! and list form land in Phase 2.

use anyhow::{Context, Result, bail};
use semver::{Version, VersionReq};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Flavor {
    Saas,
    Software,
}

impl fmt::Display for Flavor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Flavor::Saas => f.write_str("saas"),
            Flavor::Software => f.write_str("software"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Deployment {
    Saas,
    Software { version: Version },
}

impl Deployment {
    pub fn flavor(&self) -> Flavor {
        match self {
            Deployment::Saas => Flavor::Saas,
            Deployment::Software { .. } => Flavor::Software,
        }
    }

    pub fn matches(&self, constraint: &DeploymentConstraint) -> bool {
        if self.flavor() != constraint.flavor {
            return false;
        }
        match (self, &constraint.version) {
            (Deployment::Saas, _) => true,
            (Deployment::Software { version }, Some(req)) => req.matches(version),
            (Deployment::Software { .. }, None) => true,
        }
    }
}

impl fmt::Display for Deployment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Deployment::Saas => f.write_str("saas"),
            Deployment::Software { version } => write!(f, "software-{}", version),
        }
    }
}

impl FromStr for Deployment {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        let s = s.trim();
        if s == "saas" {
            return Ok(Deployment::Saas);
        }
        if let Some(rest) = s.strip_prefix("software-") {
            let version = Version::parse(rest).with_context(|| format!("invalid deployment '{}': expected 'saas' or 'software-X.Y.Z'", s))?;
            return Ok(Deployment::Software { version });
        }
        bail!("invalid deployment '{}': expected 'saas' or 'software-X.Y.Z'", s);
    }
}

impl Serialize for Deployment {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Deployment {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(de)?;
        Deployment::from_str(&raw).map_err(serde::de::Error::custom)
    }
}

/// A single deployment constraint. Phase 1 supports the single-clause forms only:
///   "saas"
///   "software"
///   "software-5.3.x" / "software->=5.3, <6" (passed as a single VersionReq)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeploymentConstraint {
    pub flavor: Flavor,
    /// `None` for SaaS or for `software` (any version).
    pub version: Option<VersionReq>,
}

impl FromStr for DeploymentConstraint {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        let s = s.trim();
        if s == "saas" {
            return Ok(Self { flavor: Flavor::Saas, version: None });
        }
        if s == "software" {
            return Ok(Self { flavor: Flavor::Software, version: None });
        }
        if let Some(rest) = s.strip_prefix("software-") {
            // Accept "5.3.x", "5.3.0", ">=5.3, <6". semver::VersionReq parses all of these.
            let req = VersionReq::parse(rest).with_context(|| format!("invalid software constraint '{}': expected a semver requirement after 'software-'", s))?;
            return Ok(Self { flavor: Flavor::Software, version: Some(req) });
        }
        bail!("invalid deployment constraint '{}': expected 'saas', 'software', or 'software-<req>'", s);
    }
}

impl fmt::Display for DeploymentConstraint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.flavor, &self.version) {
            (Flavor::Saas, _) => f.write_str("saas"),
            (Flavor::Software, None) => f.write_str("software"),
            (Flavor::Software, Some(req)) => write!(f, "software-{}", req),
        }
    }
}

/// One or more `DeploymentConstraint`s combined with logical OR.
/// Schema `deployments:` keys and `metadata.requires.deployment` accept either
/// a single string or a YAML/JSON array of strings.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DeploymentConstraintList(pub Vec<DeploymentConstraint>);

impl DeploymentConstraintList {
    pub fn matches(&self, deployment: &Deployment) -> bool {
        self.0.iter().any(|c| deployment.matches(c))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl FromStr for DeploymentConstraintList {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        let s = s.trim();
        if s.is_empty() {
            return Ok(Self::default());
        }
        // Accept comma-separated lists where each clause is itself a constraint.
        // The `software-X` constraint may legally contain commas inside its semver
        // requirement (e.g. ">=5.3, <6"), so we cannot blindly split on `,`.
        // Strategy: split on top-level commas only — a clause boundary is a comma
        // immediately followed by `saas` or `software` (after whitespace).
        let mut parts: Vec<String> = Vec::new();
        let mut buf = String::new();
        for (i, c) in s.char_indices() {
            if c == ',' {
                let rest = s[i + 1..].trim_start();
                if rest.starts_with("saas") || rest.starts_with("software") {
                    parts.push(buf.trim().to_string());
                    buf.clear();
                    continue;
                }
            }
            buf.push(c);
        }
        if !buf.trim().is_empty() {
            parts.push(buf.trim().to_string());
        }
        let constraints = parts.iter().map(|p| DeploymentConstraint::from_str(p)).collect::<Result<Vec<_>>>()?;
        Ok(Self(constraints))
    }
}

impl fmt::Display for DeploymentConstraintList {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let parts: Vec<String> = self.0.iter().map(|c| c.to_string()).collect();
        f.write_str(&parts.join(", "))
    }
}

impl Serialize for DeploymentConstraintList {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        if self.0.len() == 1 {
            ser.collect_str(&self.0[0])
        } else {
            use serde::ser::SerializeSeq;
            let mut seq = ser.serialize_seq(Some(self.0.len()))?;
            for c in &self.0 {
                seq.serialize_element(&c.to_string())?;
            }
            seq.end()
        }
    }
}

impl<'de> Deserialize<'de> for DeploymentConstraintList {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        // Accept either a single string or a sequence of strings.
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            One(String),
            Many(Vec<String>),
        }
        let raw = Raw::deserialize(de)?;
        match raw {
            Raw::One(s) => DeploymentConstraintList::from_str(&s).map_err(serde::de::Error::custom),
            Raw::Many(items) => {
                let constraints = items.iter().map(|s| DeploymentConstraint::from_str(s)).collect::<Result<Vec<_>>>().map_err(serde::de::Error::custom)?;
                Ok(Self(constraints))
            }
        }
    }
}

impl Serialize for DeploymentConstraint {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for DeploymentConstraint {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(de)?;
        DeploymentConstraint::from_str(&raw).map_err(serde::de::Error::custom)
    }
}

/// Choose the most specific deployment-overlay key that matches `deployment`.
///
/// Specificity ranking:
///   1. Exact `"saas"`               -> only matches Saas, specificity 100
///   2. `"software-X.Y.Z"`          -> exact version match, specificity 90
///   3. `"software-X.Y"`             -> minor-version constraint, specificity 70
///   4. `"software-X"`               -> major-version constraint, specificity 50
///   5. `"software-<other>"`         -> arbitrary semver req, specificity 30
///   6. `"software"`                 -> any software, specificity 10
///
/// Returns the matching key with the highest specificity, or `None` when no
/// key in `keys` matches `deployment`. Ties are broken by lexical key order
/// (deterministic so the same input always yields the same overlay).
pub fn select_overlay_key<'k, I>(keys: I, deployment: &Deployment) -> Option<&'k str>
where
    I: IntoIterator<Item = &'k str>,
{
    let mut best: Option<(u32, &str)> = None;
    for key in keys {
        let constraint = match DeploymentConstraint::from_str(key) {
            Ok(c) => c,
            Err(_) => continue,
        };
        if !deployment.matches(&constraint) {
            continue;
        }
        let specificity = score_specificity(key);
        match best {
            None => best = Some((specificity, key)),
            Some((cur, cur_key)) => {
                if specificity > cur || (specificity == cur && key < cur_key) {
                    best = Some((specificity, key));
                }
            }
        }
    }
    best.map(|(_, k)| k)
}

fn score_specificity(key: &str) -> u32 {
    if key == "saas" {
        return 100;
    }
    if key == "software" {
        return 10;
    }
    let Some(rest) = key.strip_prefix("software-") else {
        return 0;
    };
    // Treat the suffix as a semver constraint to probe its shape.
    if Version::parse(rest).is_ok() {
        return 90;
    }
    let dots = rest.matches('.').count();
    let has_x_or_star = rest.contains('x') || rest.contains('*') || rest.contains('?');
    let starts_with_op = rest.starts_with('>') || rest.starts_with('<') || rest.starts_with('=') || rest.starts_with('~') || rest.starts_with('^');
    if starts_with_op {
        return 30;
    }
    if has_x_or_star {
        // "5.3.x" -> 2 dots, "5.x" -> 1 dot
        return if dots >= 2 { 70 } else { 50 };
    }
    // Bare "5.3" or "5" — treat as version-prefix constraint.
    if dots >= 1 { 70 } else { 50 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sw(v: &str) -> Deployment {
        Deployment::Software { version: Version::parse(v).unwrap() }
    }

    #[test]
    fn deployment_from_str_parse_and_display() {
        // Accept: saas + software-X.Y.Z (round-trips through Display).
        assert_eq!("saas".parse::<Deployment>().unwrap(), Deployment::Saas);
        let d: Deployment = "software-5.3.0".parse().unwrap();
        assert_eq!(d, sw("5.3.0"));
        assert_eq!(d.to_string(), "software-5.3.0");

        // Reject: a concrete Deployment needs an explicit version — bare "software",
        // empty suffix, and non-numeric versions are all errors.
        for bad in ["foo", "software", "software-", "software-x.y.z"] {
            assert!(bad.parse::<Deployment>().is_err(), "{bad} must not parse as a Deployment");
        }
    }

    #[test]
    fn constraint_matches_across_flavors_and_versions() {
        // Each row: (constraint string, deployment, should_match, why).
        let cases: &[(&str, Deployment, bool)] = &[
            // "saas" matches only Saas.
            ("saas", Deployment::Saas, true),
            ("saas", sw("5.3.0"), false),
            // bare "software" matches any software version, never saas.
            ("software", sw("5.3.0"), true),
            ("software", sw("6.0.0"), true),
            ("software", Deployment::Saas, false),
            // "software-5.3.x" is a version req: 5.3.* matches, 5.4 does not.
            ("software-5.3.x", sw("5.3.0"), true),
            ("software-5.3.x", sw("5.3.7"), true),
            ("software-5.3.x", sw("5.4.0"), false),
        ];
        for (cstr, dep, want) in cases {
            let c: DeploymentConstraint = cstr.parse().unwrap();
            assert_eq!(dep.matches(&c), *want, "constraint {cstr} vs {dep:?}");
        }
    }

    #[test]
    fn serde_roundtrip() {
        let d = sw("5.3.0");
        let json = serde_json::to_string(&d).unwrap();
        assert_eq!(json, "\"software-5.3.0\"");
        let back: Deployment = serde_json::from_str(&json).unwrap();
        assert_eq!(back, d);
    }

    #[test]
    fn constraint_list_parses_clauses_and_matches() {
        // Single clause.
        let single: DeploymentConstraintList = "software-5.3.x".parse().unwrap();
        assert_eq!(single.0.len(), 1);
        assert!(single.matches(&sw("5.3.0")));

        // Comma-separated distinct clauses → OR over flavors/versions.
        let multi: DeploymentConstraintList = "saas, software-5.3.x".parse().unwrap();
        assert_eq!(multi.0.len(), 2);
        assert!(multi.matches(&Deployment::Saas));
        assert!(multi.matches(&sw("5.3.7")));
        assert!(!multi.matches(&sw("5.4.0")));

        // ">=5.3, <6" is a single semver req inside one software- clause — the
        // internal comma must NOT split into two clauses.
        let internal: DeploymentConstraintList = "software->=5.3, <6".parse().unwrap();
        assert_eq!(internal.0.len(), 1);
        assert!(internal.matches(&sw("5.3.5")));
        assert!(!internal.matches(&sw("6.0.0")));
    }

    #[test]
    fn constraint_list_serde_string_and_array_forms() {
        #[derive(Deserialize)]
        struct W {
            deployment: DeploymentConstraintList,
        }
        // String form → one clause.
        let s: W = serde_norway::from_str("deployment: software-5.3.x\n").unwrap();
        assert_eq!(s.deployment.0.len(), 1);
        // Array form → one clause per element.
        let a: W = serde_norway::from_str("deployment: [saas, software-5.3.x]\n").unwrap();
        assert_eq!(a.deployment.0.len(), 2);
    }

    #[test]
    fn select_overlay_key_specificity() {
        // Each row: (keys, active deployment, expected winner, why).
        let cases: Vec<(Vec<&str>, Deployment, Option<&str>)> = vec![
            // Most-specific matching key wins: exact minor beats generic "software".
            (vec!["software", "software-5.3"], sw("5.3.0"), Some("software-5.3")),
            // Unmatched minor → fall back to generic "software".
            (vec!["software", "software-5.3"], sw("6.0.0"), Some("software")),
            // No key matches saas → None.
            (vec!["software-5.3"], Deployment::Saas, None),
            // Exact version beats both range and generic.
            (vec!["software", "software-5.3.0", "software-5.3"], sw("5.3.0"), Some("software-5.3.0")),
        ];
        for (keys, active, want) in cases {
            assert_eq!(select_overlay_key(keys.iter().copied(), &active), want, "keys {keys:?} vs {active:?}");
        }
    }
}
