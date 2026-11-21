use std::fs;
use std::path::Path;

use zed_extension_api::{self as zed, LanguageServerId, Result, settings::LspSettings};

pub const SERVER_ID: &str = "flutter-debugger";
pub const BINARY_NAME: &str = "flutter-debugger-server";
pub const GITHUB_REPO: &str = "Matkurban/flutter-debugger";

#[derive(Default)]
pub struct BinaryResolver {
    cached_binary_path: Option<String>,
}

impl BinaryResolver {
    pub fn resolve_for_lsp(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<String> {
        if let Ok(settings) = LspSettings::for_worktree(SERVER_ID, worktree)
            && let Some(binary) = settings.binary
            && let Some(path) = binary.path
            && !path.trim().is_empty()
        {
            return Ok(path);
        }

        self.resolve_common(Some(language_server_id), worktree)
    }

    pub fn resolve_for_dap(
        &mut self,
        user_provided_path: Option<String>,
        worktree: &zed::Worktree,
    ) -> Result<String> {
        if let Some(path) = user_provided_path
            && !path.trim().is_empty()
        {
            return Ok(path);
        }

        if let Ok(settings) = LspSettings::for_worktree(SERVER_ID, worktree)
            && let Some(binary) = settings.binary
            && let Some(path) = binary.path
            && !path.trim().is_empty()
        {
            return Ok(path);
        }

        self.resolve_common(None, worktree)
    }

    fn resolve_common(
        &mut self,
        language_server_id: Option<&LanguageServerId>,
        worktree: &zed::Worktree,
    ) -> Result<String> {
        if let Some(path) = worktree.which(BINARY_NAME) {
            return Ok(path);
        }

        if let Some(path) = &self.cached_binary_path
            && Path::new(path).is_file()
        {
            return Ok(path.clone());
        }

        if let Some(id) = language_server_id {
            zed::set_language_server_installation_status(
                id,
                &zed::LanguageServerInstallationStatus::CheckingForUpdate,
            );
        }

        let release = zed::latest_github_release(
            GITHUB_REPO,
            zed::GithubReleaseOptions {
                require_assets: true,
                pre_release: false,
            },
        )?;

        let (platform, arch) = zed::current_platform();
        let (asset_name, file_type, exe_name) = asset_spec(platform, arch)?;

        let asset = release
            .assets
            .iter()
            .find(|asset| asset.name == asset_name)
            .ok_or_else(|| {
                format!(
                    "No release asset named `{asset_name}` found in {GITHUB_REPO} release `{}`",
                    release.version
                )
            })?;

        let version_dir = format!("{BINARY_NAME}-{}", release.version);
        let binary_path = format!("{version_dir}/{exe_name}");

        if !Path::new(&binary_path).is_file() {
            if let Some(id) = language_server_id {
                zed::set_language_server_installation_status(
                    id,
                    &zed::LanguageServerInstallationStatus::Downloading,
                );
            }

            zed::download_file(&asset.download_url, &version_dir, file_type)
                .map_err(|err| format!("Failed to download `{asset_name}`: {err}"))?;

            zed::make_file_executable(&binary_path)?;

            cleanup_old_versions(&version_dir);
        }

        if let Some(id) = language_server_id {
            zed::set_language_server_installation_status(
                id,
                &zed::LanguageServerInstallationStatus::None,
            );
        }

        self.cached_binary_path = Some(binary_path.clone());
        Ok(binary_path)
    }
}

fn asset_spec(
    platform: zed::Os,
    arch: zed::Architecture,
) -> Result<(String, zed::DownloadedFileType, &'static str)> {
    let os_str = match platform {
        zed::Os::Mac => "macos",
        zed::Os::Linux => "linux",
        zed::Os::Windows => "windows",
    };

    let arch_str = match arch {
        zed::Architecture::Aarch64 => "aarch64",
        zed::Architecture::X8664 => "x86_64",
        zed::Architecture::X86 => {
            return Err("32-bit x86 architecture is not supported".to_string());
        }
    };

    let (ext, file_type, exe_name) = match platform {
        zed::Os::Windows => ("zip", zed::DownloadedFileType::Zip, "flutter-debugger-server.exe"),
        zed::Os::Mac | zed::Os::Linux => {
            ("tar.gz", zed::DownloadedFileType::GzipTar, BINARY_NAME)
        }
    };

    Ok((
        format!("{BINARY_NAME}-{os_str}-{arch_str}.{ext}"),
        file_type,
        exe_name,
    ))
}

fn cleanup_old_versions(current_version_dir: &str) {
    let Ok(entries) = fs::read_dir(".") else {
        return;
    };

    let prefix = format!("{BINARY_NAME}-");
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        if name_str.starts_with(&prefix) && name_str != current_version_dir {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}
