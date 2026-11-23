use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Deserialize;

use crate::backend::models::SourceLocation;

#[derive(Debug, Deserialize)]
struct PackageConfigFile {
    #[serde(default)]
    packages: Vec<PackageConfigEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PackageConfigEntry {
    name: String,
    root_uri: String,
    #[serde(default)]
    package_uri: Option<String>,
}

/// Resolves a Dart `file:///...` or `package:<pkg>/<rel>.dart` URI to a local disk path.
/// For `package:` URIs, reads `<project_root>/.dart_tool/package_config.json` to map package roots.
pub fn resolve_dart_uri_to_local_path(uri: &str, project_root: Option<&Path>) -> Option<PathBuf> {
    let trimmed = uri.trim();
    if trimmed.starts_with("file://") {
        if let Ok(parsed) = url::Url::parse(trimmed)
            && let Ok(path) = parsed.to_file_path()
        {
            return Some(path);
        }
        let stripped = trimmed.strip_prefix("file://")?;
        return Some(PathBuf::from(stripped));
    }

    if let Some(pkg_spec) = trimmed.strip_prefix("package:") {
        let (pkg_name, rel_path) = pkg_spec.split_once('/')?;
        let root = project_root?;
        let config_dir = root.join(".dart_tool");
        let config_path = config_dir.join("package_config.json");
        let bytes = fs::read(&config_path).ok()?;
        let parsed: PackageConfigFile = serde_json::from_slice(&bytes).ok()?;

        let entry = parsed.packages.iter().find(|p| p.name == pkg_name)?;
        let pkg_root = resolve_package_root_uri(&entry.root_uri, &config_dir)?;
        let package_sub = entry
            .package_uri
            .as_deref()
            .unwrap_or("lib/")
            .trim_end_matches('/');

        let full_path = if package_sub.is_empty() || package_sub == "." {
            pkg_root.join(rel_path)
        } else {
            pkg_root.join(package_sub).join(rel_path)
        };

        return Some(clean_relative_segments(&full_path));
    }

    let candidate = Path::new(trimmed);
    if candidate.is_absolute() {
        return Some(candidate.to_path_buf());
    }
    project_root.map(|r| r.join(candidate))
}

fn resolve_package_root_uri(root_uri: &str, config_dir: &Path) -> Option<PathBuf> {
    if root_uri.starts_with("file://") {
        if let Ok(parsed) = url::Url::parse(root_uri)
            && let Ok(path) = parsed.to_file_path()
        {
            return Some(path);
        }
        return root_uri.strip_prefix("file://").map(PathBuf::from);
    }
    Some(clean_relative_segments(&config_dir.join(root_uri)))
}

fn clean_relative_segments(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Opens `<abs_path>:<line>:<col>` in Zed editor (`zed` in `$PATH` or `/Applications/Zed.app/Contents/MacOS/cli`).
pub fn open_source_in_zed(
    loc: &SourceLocation,
    project_root: Option<&Path>,
) -> Result<String, String> {
    let resolved = resolve_dart_uri_to_local_path(&loc.uri, project_root)
        .ok_or_else(|| format!("Could not resolve URI `{}` to a local file", loc.uri))?;

    let target = format!("{}:{}:{}", resolved.display(), loc.line.max(1), loc.column.max(1));
    let candidates = [
        "zed",
        "/Applications/Zed.app/Contents/MacOS/cli",
        "/Applications/Zed Preview.app/Contents/MacOS/cli",
        "/usr/local/bin/zed",
    ];

    for bin in candidates {
        if bin.starts_with('/') && !Path::new(bin).exists() {
            continue;
        }
        if let Ok(mut child) = Command::new(bin)
            .arg(&target)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            return Ok(format!("Opened in Zed: {target}"));
        }
    }

    Err(format!(
        "Could not find `zed` CLI to open `{target}`. Run `zed: install cli` in Zed."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_file_and_package_uris_via_package_config() {
        let dir = tempfile::tempdir().expect("tempdir");
        let project_root = dir.path().join("my_flutter_app");
        let dart_tool = project_root.join(".dart_tool");
        fs::create_dir_all(&dart_tool).expect("mkdir .dart_tool");

        let external_pkg = dir.path().join("pub_cache").join("provider-6.1.0");
        fs::create_dir_all(external_pkg.join("lib").join("src")).expect("mkdir provider");

        let pkg_config = serde_json::json!({
            "configVersion": 2,
            "packages": [
                {
                    "name": "my_flutter_app",
                    "rootUri": "../",
                    "packageUri": "lib/"
                },
                {
                    "name": "provider",
                    "rootUri": format!("file://{}", external_pkg.display()),
                    "packageUri": "lib/"
                }
            ]
        });
        fs::write(
            dart_tool.join("package_config.json"),
            serde_json::to_vec_pretty(&pkg_config).unwrap(),
        )
        .unwrap();

        let local_resolved = resolve_dart_uri_to_local_path(
            "package:my_flutter_app/screens/home_page.dart",
            Some(&project_root),
        )
        .expect("resolve local package");
        assert_eq!(
            local_resolved,
            project_root.join("lib/screens/home_page.dart")
        );

        let ext_resolved = resolve_dart_uri_to_local_path(
            "package:provider/src/provider.dart",
            Some(&project_root),
        )
        .expect("resolve external package");
        assert_eq!(ext_resolved, external_pkg.join("lib/src/provider.dart"));

        let file_resolved = resolve_dart_uri_to_local_path(
            "file:///Users/dev/project/lib/main.dart",
            Some(&project_root),
        )
        .expect("resolve file uri");
        assert_eq!(
            file_resolved,
            PathBuf::from("/Users/dev/project/lib/main.dart")
        );
    }
}
