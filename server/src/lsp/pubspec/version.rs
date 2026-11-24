use std::cmp::Ordering;

use semver::Version;

/// A Dart pub version constraint (supports `any`, `^1.2.3`, `>=1.0.0 <2.0.0`, exact `1.2.3`, etc.).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionConstraint {
    Any,
    Range(VersionRange),
    Invalid(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionRange {
    pub min: Option<Bound>,
    pub max: Option<Bound>,
    /// True if the original constraint explicitly referenced a prerelease version.
    pub allows_prerelease: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bound {
    pub version: Version,
    pub inclusive: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpgradeKind {
    UpToDate,
    Patch,
    Minor,
    Major,
    Prerelease,
}

impl VersionConstraint {
    /// Parses a Dart pub version constraint string according to the official Dart pubspec specification.
    #[must_use]
    pub fn parse(raw: &str) -> Self {
        let trimmed = strip_quotes(raw).trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("any") {
            return Self::Any;
        }

        if let Some(rest) = trimmed.strip_prefix('^') {
            return match parse_dart_version(rest.trim()) {
                Some(min_ver) => {
                    let max_ver = caret_upper_bound(&min_ver);
                    let allows_pre = !min_ver.pre.is_empty();
                    Self::Range(VersionRange {
                        min: Some(Bound {
                            version: min_ver,
                            inclusive: true,
                        }),
                        max: Some(Bound {
                            version: max_ver,
                            inclusive: false,
                        }),
                        allows_prerelease: allows_pre,
                    })
                }
                None => Self::Invalid(trimmed.to_string()),
            };
        }

        let tokens = tokenize_constraint(trimmed);
        if tokens.is_empty() {
            return Self::Any;
        }

        let mut min: Option<Bound> = None;
        let mut max: Option<Bound> = None;
        let mut allows_prerelease = false;

        for token in tokens {
            if let Some(ver_str) = token.strip_prefix(">=") {
                let Some(ver) = parse_dart_version(ver_str) else {
                    return Self::Invalid(trimmed.to_string());
                };
                if !ver.pre.is_empty() {
                    allows_prerelease = true;
                }
                min = tighten_min(
                    min,
                    Bound {
                        version: ver,
                        inclusive: true,
                    },
                );
            } else if let Some(ver_str) = token.strip_prefix("<=") {
                let Some(ver) = parse_dart_version(ver_str) else {
                    return Self::Invalid(trimmed.to_string());
                };
                if !ver.pre.is_empty() {
                    allows_prerelease = true;
                }
                max = tighten_max(
                    max,
                    Bound {
                        version: ver,
                        inclusive: true,
                    },
                );
            } else if let Some(ver_str) = token.strip_prefix('>') {
                let Some(ver) = parse_dart_version(ver_str) else {
                    return Self::Invalid(trimmed.to_string());
                };
                if !ver.pre.is_empty() {
                    allows_prerelease = true;
                }
                min = tighten_min(
                    min,
                    Bound {
                        version: ver,
                        inclusive: false,
                    },
                );
            } else if let Some(ver_str) = token.strip_prefix('<') {
                let Some(ver) = parse_dart_version(ver_str) else {
                    return Self::Invalid(trimmed.to_string());
                };
                if !ver.pre.is_empty() {
                    allows_prerelease = true;
                }
                max = tighten_max(
                    max,
                    Bound {
                        version: ver,
                        inclusive: false,
                    },
                );
            } else {
                let Some(ver) = parse_dart_version(&token) else {
                    return Self::Invalid(trimmed.to_string());
                };
                if !ver.pre.is_empty() {
                    allows_prerelease = true;
                }
                min = tighten_min(
                    min,
                    Bound {
                        version: ver.clone(),
                        inclusive: true,
                    },
                );
                max = tighten_max(
                    max,
                    Bound {
                        version: ver,
                        inclusive: true,
                    },
                );
            }
        }

        Self::Range(VersionRange {
            min,
            max,
            allows_prerelease,
        })
    }

    #[must_use]
    pub fn matches(&self, version: &Version) -> bool {
        match self {
            Self::Any => version.pre.is_empty(),
            Self::Invalid(_) => false,
            Self::Range(range) => range.matches(version),
        }
    }

    #[must_use]
    pub fn baseline_version(&self) -> Option<&Version> {
        match self {
            Self::Range(r) => r.min.as_ref().map(|b| &b.version),
            Self::Any | Self::Invalid(_) => None,
        }
    }

    #[must_use]
    pub fn opts_into_prerelease(&self) -> bool {
        matches!(self, Self::Range(r) if r.allows_prerelease)
    }
}

impl VersionRange {
    #[must_use]
    pub fn matches(&self, version: &Version) -> bool {
        if let Some(min) = &self.min {
            match compare_dart_versions(version, &min.version) {
                Ordering::Less => return false,
                Ordering::Equal if !min.inclusive => return false,
                _ => {}
            }
        }
        if let Some(max) = &self.max {
            match compare_dart_versions(version, &max.version) {
                Ordering::Greater => return false,
                Ordering::Equal if !max.inclusive => return false,
                _ => {}
            }
        }
        version.pre.is_empty() || self.allows_prerelease
    }
}

/// Parses a Dart SemVer string, normalizing 2-part versions like `3.5` to `3.5.0`.
#[must_use]
pub fn parse_dart_version(input: &str) -> Option<Version> {
    let trimmed = input.trim();
    if let Ok(v) = Version::parse(trimmed) {
        return Some(v);
    }
    let (core, suffix) = match trimmed.find(['-', '+']) {
        Some(idx) => (&trimmed[..idx], &trimmed[idx..]),
        None => (trimmed, ""),
    };
    let dot_count = core.bytes().filter(|&b| b == b'.').count();
    if dot_count == 1 {
        Version::parse(&format!("{core}.0{suffix}")).ok()
    } else {
        None
    }
}

/// Compares two Dart versions (including build metadata per Dart's SemVer 2.0.0-rc.1 rule).
#[must_use]
pub fn compare_dart_versions(a: &Version, b: &Version) -> Ordering {
    let base = a.cmp(b);
    if base != Ordering::Equal {
        return base;
    }
    a.build.as_str().cmp(b.build.as_str())
}

/// Computes the exclusive upper bound for Dart caret syntax (`^version`).
#[must_use]
pub fn caret_upper_bound(min: &Version) -> Version {
    if min.major > 0 {
        Version::new(min.major + 1, 0, 0)
    } else if min.minor > 0 {
        Version::new(0, min.minor + 1, 0)
    } else {
        Version::new(0, 0, min.patch + 1)
    }
}

/// Classifies the upgrade distance from `baseline` to `target`.
#[must_use]
pub fn classify_upgrade(baseline: &Version, target: &Version) -> UpgradeKind {
    if compare_dart_versions(target, baseline) != Ordering::Greater {
        return UpgradeKind::UpToDate;
    }
    if !target.pre.is_empty() && baseline.pre.is_empty() {
        return UpgradeKind::Prerelease;
    }
    if target.major > baseline.major {
        UpgradeKind::Major
    } else if target.minor > baseline.minor {
        if baseline.major == 0 {
            UpgradeKind::Major
        } else {
            UpgradeKind::Minor
        }
    } else {
        UpgradeKind::Patch
    }
}

/// Formats an upgraded version constraint while preserving the user's quote and caret style.
#[must_use]
pub fn format_upgraded_constraint(original_raw: Option<&str>, new_version: &Version) -> String {
    let Some(raw) = original_raw else {
        return format!("^{new_version}");
    };
    let trimmed = raw.trim();
    let quote = if trimmed.starts_with('\'') && trimmed.ends_with('\'') && trimmed.len() >= 2 {
        Some('\'')
    } else if trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2 {
        Some('"')
    } else {
        None
    };

    let inner = strip_quotes(trimmed).trim();
    let is_exact = !inner.is_empty()
        && !inner.eq_ignore_ascii_case("any")
        && !inner.starts_with('^')
        && !inner.starts_with('>')
        && !inner.starts_with('<');

    let formatted = if is_exact {
        new_version.to_string()
    } else {
        format!("^{new_version}")
    };

    match quote {
        Some(q) => format!("{q}{formatted}{q}"),
        None => formatted,
    }
}

/// Tightens a dependency constraint's lower bound to match `locked_version` (analogous to
/// `dart pub upgrade --tighten`). For compound range constraints such as `>=5.4.0 <7.0.0`,
/// only the lower bound is raised while preserving the upper bound and quotes; for caret/exact
/// constraints, delegates to [`format_upgraded_constraint`].
#[must_use]
pub fn format_tightened_constraint(original_raw: Option<&str>, locked_version: &Version) -> String {
    let Some(raw) = original_raw else {
        return format!("^{locked_version}");
    };
    let trimmed = raw.trim();
    let quote = if trimmed.starts_with('\'') && trimmed.ends_with('\'') && trimmed.len() >= 2 {
        Some('\'')
    } else if trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2 {
        Some('"')
    } else {
        None
    };

    let inner = strip_quotes(trimmed).trim();
    let tokens = tokenize_constraint(inner);
    let has_lower = tokens
        .iter()
        .any(|t| t.starts_with(">=") || t.starts_with('>'));
    let has_upper = tokens
        .iter()
        .any(|t| t.starts_with("<=") || t.starts_with('<'));

    if has_lower && has_upper {
        let updated: Vec<String> = tokens
            .into_iter()
            .map(|t| {
                if t.starts_with(">=") || t.starts_with('>') {
                    format!(">={locked_version}")
                } else {
                    t
                }
            })
            .collect();
        let joined = updated.join(" ");
        return match quote {
            Some(q) => format!("{q}{joined}{q}"),
            None => joined,
        };
    }

    format_upgraded_constraint(original_raw, locked_version)
}

/// Checks whether a candidate package version's `environment.sdk` constraint is compatible
/// with the current project's `environment.sdk` constraint.
#[must_use]
pub fn is_sdk_compatible(
    project_sdk: Option<&VersionConstraint>,
    pkg_sdk_raw: Option<&str>,
) -> bool {
    let (Some(proj), Some(pkg_raw)) = (project_sdk, pkg_sdk_raw) else {
        return true;
    };
    let pkg = VersionConstraint::parse(pkg_raw);
    let (VersionConstraint::Range(proj_r), VersionConstraint::Range(pkg_r)) = (proj, &pkg) else {
        return true;
    };

    if let (Some(pkg_min), Some(proj_max)) = (&pkg_r.min, &proj_r.max)
        && compare_dart_versions(&pkg_min.version, &proj_max.version) != Ordering::Less
    {
        return false;
    }
    if let (Some(pkg_max), Some(proj_min)) = (&pkg_r.max, &proj_r.min)
        && compare_dart_versions(&pkg_max.version, &proj_min.version) != Ordering::Greater
    {
        return false;
    }
    if let (Some(pkg_min), Some(proj_min)) = (&pkg_r.min, &proj_r.min)
        && compare_dart_versions(&pkg_min.version, &proj_min.version) == Ordering::Greater
    {
        return false;
    }
    true
}

#[must_use]
pub fn strip_quotes(input: &str) -> &str {
    let s = input.trim();
    if s.len() >= 2
        && ((s.starts_with('\'') && s.ends_with('\'')) || (s.starts_with('"') && s.ends_with('"')))
    {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

fn tokenize_constraint(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut iter = input.split_whitespace().peekable();
    while let Some(part) = iter.next() {
        if matches!(part, ">=" | "<=" | ">" | "<") {
            if let Some(next) = iter.next() {
                tokens.push(format!("{part}{next}"));
            }
        } else {
            tokens.push(part.to_string());
        }
    }
    tokens
}

fn tighten_min(existing: Option<Bound>, candidate: Bound) -> Option<Bound> {
    match existing {
        None => Some(candidate),
        Some(prev) => match compare_dart_versions(&candidate.version, &prev.version) {
            Ordering::Greater => Some(candidate),
            Ordering::Less => Some(prev),
            Ordering::Equal => Some(Bound {
                version: prev.version,
                inclusive: prev.inclusive && candidate.inclusive,
            }),
        },
    }
}

fn tighten_max(existing: Option<Bound>, candidate: Bound) -> Option<Bound> {
    match existing {
        None => Some(candidate),
        Some(prev) => match compare_dart_versions(&candidate.version, &prev.version) {
            Ordering::Less => Some(candidate),
            Ordering::Greater => Some(prev),
            Ordering::Equal => Some(Bound {
                version: prev.version,
                inclusive: prev.inclusive && candidate.inclusive,
            }),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluates_dart_caret_and_range_constraints() {
        let caret_major = VersionConstraint::parse("^1.3.0");
        assert!(caret_major.matches(&parse_dart_version("1.3.0").expect("v")));
        assert!(caret_major.matches(&parse_dart_version("1.9.9").expect("v")));
        assert!(!caret_major.matches(&parse_dart_version("2.0.0").expect("v")));

        let caret_zero = VersionConstraint::parse("^0.1.2");
        assert!(caret_zero.matches(&parse_dart_version("0.1.9").expect("v")));
        assert!(!caret_zero.matches(&parse_dart_version("0.2.0").expect("v")));

        let caret_patch = VersionConstraint::parse("^0.0.3");
        assert!(caret_patch.matches(&parse_dart_version("0.0.3").expect("v")));
        assert!(!caret_patch.matches(&parse_dart_version("0.0.4").expect("v")));

        let range = VersionConstraint::parse("'>=1.2.0 <2.0.0'");
        assert!(range.matches(&parse_dart_version("1.5.0").expect("v")));
        assert!(!range.matches(&parse_dart_version("2.0.0").expect("v")));
    }

    #[test]
    fn classifies_upgrades_preserves_quotes_and_checks_sdk_bounds() {
        let v1 = parse_dart_version("1.2.3").expect("v");
        let v1_patch = parse_dart_version("1.2.4").expect("v");
        let v1_minor = parse_dart_version("1.3.0").expect("v");
        let v2 = parse_dart_version("2.0.0").expect("v");
        let v0_1 = parse_dart_version("0.1.0").expect("v");
        let v0_2 = parse_dart_version("0.2.0").expect("v");

        assert_eq!(classify_upgrade(&v1, &v1_patch), UpgradeKind::Patch);
        assert_eq!(classify_upgrade(&v1, &v1_minor), UpgradeKind::Minor);
        assert_eq!(classify_upgrade(&v1, &v2), UpgradeKind::Major);
        assert_eq!(classify_upgrade(&v0_1, &v0_2), UpgradeKind::Major);

        assert_eq!(format_upgraded_constraint(Some("^1.2.3"), &v2), "^2.0.0");
        assert_eq!(format_upgraded_constraint(Some("'^1.2.3'"), &v2), "'^2.0.0'");
        assert_eq!(
            format_upgraded_constraint(Some("\"1.2.3\""), &v2),
            "\"2.0.0\""
        );

        let project_sdk = VersionConstraint::parse("^3.5.0");
        assert!(is_sdk_compatible(Some(&project_sdk), Some("^3.2.0")));
        assert!(!is_sdk_compatible(Some(&project_sdk), Some("^3.8.0")));
        assert!(!is_sdk_compatible(Some(&project_sdk), Some(">=2.12.0 <3.0.0")));

        let v59 = parse_dart_version("5.9.0").expect("v");
        assert_eq!(format_tightened_constraint(Some("^5.4.0"), &v59), "^5.9.0");
        assert_eq!(
            format_tightened_constraint(Some("'^5.4.0'"), &v59),
            "'^5.9.0'"
        );
        assert_eq!(
            format_tightened_constraint(Some(">=5.4.0 <7.0.0"), &v59),
            ">=5.9.0 <7.0.0"
        );
        assert_eq!(
            format_tightened_constraint(Some("\">= 5.4.0 < 6.0.0\""), &v59),
            "\">=5.9.0 <6.0.0\""
        );
    }
}
