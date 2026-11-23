use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

pub use flutter_debugger_protocol::resolve_dart_uri_to_local_path;

use crate::backend::error::DevToolsError;
use crate::protocol::SourceLocation;

/// Builds Zed CLI arguments to open `resolved_file` at `line:col` inside an existing Zed window (`-e`):
/// - When `resolved_file` is inside `project_root`, passes `["-e", "<project_root>", "<file>:<line>:<col>"]`
///   so Zed activates the already-open workspace window for that Flutter project.
/// - When `resolved_file` is outside `project_root` (e.g. `~/.pub-cache` or Flutter SDK) or `project_root` is `None`,
///   passes `["-e", "<file>:<line>:<col>"]` so Zed opens a tab in the existing window without spawning a new window.
pub fn build_zed_open_args(
    resolved_file: &Path,
    line: u32,
    col: u32,
    project_root: Option<&Path>,
) -> Vec<String> {
    let canon_file = fs::canonicalize(resolved_file).unwrap_or_else(|_| resolved_file.to_path_buf());
    let canon_root = project_root.map(|r| fs::canonicalize(r).unwrap_or_else(|_| r.to_path_buf()));

    let target = format!(
        "{}:{}:{}",
        canon_file.display(),
        line.max(1),
        col.max(1)
    );

    if let Some(ref root) = canon_root {
        let inside_canon = canon_file.starts_with(root);
        let inside_raw = project_root.is_some_and(|raw_r| resolved_file.starts_with(raw_r));
        if inside_canon || inside_raw {
            return vec!["-e".to_string(), root.display().to_string(), target];
        }
    }

    vec!["-e".to_string(), target]
}

/// Opens `<abs_path>:<line>:<col>` in an existing Zed editor window (`zed -e`).
pub fn open_source_in_zed(
    loc: &SourceLocation,
    project_root: Option<&Path>,
) -> Result<String, DevToolsError> {
    let resolved = resolve_dart_uri_to_local_path(&loc.uri, project_root).ok_or_else(|| {
        DevToolsError::Io(format!(
            "Could not resolve URI `{}` to a local file",
            loc.uri
        ))
    })?;

    let cli_args = build_zed_open_args(&resolved, loc.line, loc.column, project_root);
    let target = cli_args.last().cloned().unwrap_or_else(|| {
        format!(
            "{}:{}:{}",
            resolved.display(),
            loc.line.max(1),
            loc.column.max(1)
        )
    });

    let candidates = [
        "zed",
        "/Applications/Zed.app/Contents/MacOS/cli",
        "/Applications/Zed Preview.app/Contents/MacOS/cli",
        "/usr/local/bin/zed",
    ];

    let chosen_bin = candidates
        .into_iter()
        .find(|&bin| {
            if bin.starts_with('/') {
                Path::new(bin).exists()
            } else {
                command_exists_in_path(bin)
            }
        })
        .map(ToOwned::to_owned);

    let Some(bin) = chosen_bin else {
        return Err(DevToolsError::Io(format!(
            "Could not find `zed` CLI to open `{target}`. Run `zed: install cli` in Zed."
        )));
    };

    std::thread::spawn(move || {
        if let Ok(out) = Command::new(&bin)
            .args(&cli_args)
            .stdin(Stdio::null())
            .output()
            && !out.status.success()
        {
            let stderr_lower = String::from_utf8_lossy(&out.stderr).to_ascii_lowercase();
            if stderr_lower.contains("unexpected argument")
                || stderr_lower.contains("unrecognized")
            {
                let fallback_args: Vec<String> = cli_args
                    .into_iter()
                    .filter(|a| a != "-e")
                    .collect();
                let _ = Command::new(&bin)
                    .args(&fallback_args)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
        }
    });

    Ok(format!("Opened in Zed: {target}"))
}

fn command_exists_in_path(cmd: &str) -> bool {
    let Some(path_os) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path_os).any(|dir| dir.join(cmd).is_file())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

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

    #[test]
    fn build_zed_open_args_handles_in_project_external_and_no_project_root() {
        let project_root = Path::new("/Users/dev/my_flutter_app");
        let in_project_file = Path::new("/Users/dev/my_flutter_app/lib/pages/home.dart");
        let external_file =
            Path::new("/Users/dev/.pub-cache/hosted/pub.dev/provider-6.1.0/lib/src/provider.dart");

        // 1. In-project file -> ["-e", "<project_root>", "<file>:<line>:<col>"]
        let args_in_project =
            build_zed_open_args(in_project_file, 42, 12, Some(project_root));
        assert_eq!(
            args_in_project,
            vec![
                "-e".to_string(),
                "/Users/dev/my_flutter_app".to_string(),
                "/Users/dev/my_flutter_app/lib/pages/home.dart:42:12".to_string(),
            ]
        );

        // 2. External pub-cache / SDK file -> ["-e", "<file>:<line>:<col>"]
        let args_external = build_zed_open_args(external_file, 19, 7, Some(project_root));
        assert_eq!(
            args_external,
            vec![
                "-e".to_string(),
                "/Users/dev/.pub-cache/hosted/pub.dev/provider-6.1.0/lib/src/provider.dart:19:7"
                    .to_string(),
            ]
        );

        // 3. No project_root -> ["-e", "<file>:<line>:<col>"]
        let args_no_root = build_zed_open_args(in_project_file, 10, 1, None);
        assert_eq!(
            args_no_root,
            vec![
                "-e".to_string(),
                "/Users/dev/my_flutter_app/lib/pages/home.dart:10:1".to_string(),
            ]
        );
    }
}
