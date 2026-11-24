use std::cmp::Ordering;

use semver::Version;

use crate::lsp::pubspec::parser::ParsedDependency;
use crate::lsp::pubspec::registry::PackageInfo;
use crate::lsp::pubspec::version::{
    UpgradeKind, VersionConstraint, classify_upgrade, compare_dart_versions,
    format_upgraded_constraint, is_sdk_compatible, parse_dart_version,
};
use crate::lsp::state::PubspecConfig;

/// Complete evaluation of a single hosted dependency against pub.dev metadata and `pubspec.lock`.
#[derive(Debug, Clone)]
pub struct DependencyEvaluation {
    pub dep: ParsedDependency,
    pub info: PackageInfo,
    pub locked_version: Option<String>,
    /// Baseline version used for comparison (locked version if newer & matching constraint, else declared min).
    pub baseline_version: Option<Version>,
    /// Latest non-retracted version satisfying the current constraint & project SDK.
    pub latest_compatible: Option<Version>,
    /// Latest non-retracted version overall (respecting prerelease policy).
    pub latest_overall: Option<Version>,
    /// Whether `latest_overall` requires a Dart SDK incompatible with the project's `environment.sdk`.
    pub latest_sdk_incompatible: bool,
    /// Required Dart SDK constraint of `latest_overall`, if any.
    pub latest_sdk_requirement: Option<String>,
    /// Whether the currently used/locked version is retracted on pub.dev.
    pub current_retracted: bool,
    pub upgrade_kind: UpgradeKind,
}

impl DependencyEvaluation {
    #[must_use]
    pub fn evaluate(
        dep: ParsedDependency,
        info: PackageInfo,
        locked_version: Option<String>,
        project_sdk: Option<&VersionConstraint>,
        config: &PubspecConfig,
    ) -> Self {
        let allow_pre = config.include_prerelease || dep.constraint.opts_into_prerelease();

        let declared_min = dep.constraint.baseline_version().cloned();
        let locked_parsed = locked_version.as_deref().and_then(parse_dart_version);

        // Prefer declared lower bound whenever `pubspec.lock` is stale (e.g. right after editing
        // `pubspec.yaml` to the latest version before `pub get` finishes), so outdated underlines
        // clear immediately. If lockfile satisfies constraint and is newer, use locked version.
        let baseline_version = match (declared_min, locked_parsed) {
            (Some(decl), Some(locked)) => {
                if dep.constraint.matches(&locked)
                    && compare_dart_versions(&locked, &decl) == Ordering::Greater
                {
                    Some(locked)
                } else {
                    Some(decl)
                }
            }
            (Some(decl), None) => Some(decl),
            (None, Some(locked)) => Some(locked),
            (None, None) => None,
        };

        let current_retracted = baseline_version.as_ref().is_some_and(|base| {
            info.versions.iter().any(|v| {
                v.retracted
                    && parse_dart_version(&v.version)
                        .is_some_and(|pv| compare_dart_versions(&pv, base) == Ordering::Equal)
            })
        });

        let mut latest_overall: Option<Version> = None;
        let mut latest_sdk_requirement: Option<String> = None;
        let mut latest_compatible: Option<Version> = None;

        // `info.versions` is sorted newest-first.
        for v in &info.versions {
            if v.retracted {
                continue;
            }
            let Some(parsed) = parse_dart_version(&v.version) else {
                continue;
            };
            if !parsed.pre.is_empty() && !allow_pre {
                continue;
            }

            if latest_overall.is_none() {
                latest_overall = Some(parsed.clone());
                latest_sdk_requirement = v.sdk_constraint.clone();
            }

            if latest_compatible.is_none()
                && dep.constraint.matches(&parsed)
                && is_sdk_compatible(project_sdk, v.sdk_constraint.as_deref())
            {
                latest_compatible = Some(parsed);
            }

            if latest_overall.is_some() && latest_compatible.is_some() {
                break;
            }
        }

        let latest_sdk_incompatible =
            !is_sdk_compatible(project_sdk, latest_sdk_requirement.as_deref());

        let upgrade_kind = match (&baseline_version, &latest_overall) {
            (Some(base), Some(latest)) => classify_upgrade(base, latest),
            _ => UpgradeKind::UpToDate,
        };

        Self {
            dep,
            info,
            locked_version,
            baseline_version,
            latest_compatible,
            latest_overall,
            latest_sdk_incompatible,
            latest_sdk_requirement,
            current_retracted,
            upgrade_kind,
        }
    }

    #[must_use]
    pub fn has_update(&self) -> bool {
        self.upgrade_kind != UpgradeKind::UpToDate
    }

    #[must_use]
    pub fn preferred_upgrade_edit(&self) -> Option<(Version, String)> {
        let target = self.latest_overall.as_ref()?;
        if !self.has_update() && !self.current_retracted {
            return None;
        }
        let new_text = format_upgraded_constraint(self.dep.raw_constraint.as_deref(), target);
        Some((target.clone(), new_text))
    }
}
