use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::json;
use tower_lsp_server::ls_types::{CodeLens, Command, Position, Range, Uri};

pub const WIZARD_FILENAME: &str = "flutter-project-wizard.json";

pub const ALLOWED_TEMPLATES: &[&str] = &[
    "app",
    "package",
    "plugin",
    "plugin_ffi",
    "module",
    "skeleton",
];

pub const ALLOWED_PLATFORMS: &[&str] = &["android", "ios", "macos", "windows", "linux", "web"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectWizardConfig {
    pub name: String,
    pub org: String,
    pub template: String,
    pub platforms: Vec<String>,
    pub description: String,
    pub output_dir: String,
    #[serde(default)]
    pub empty: bool,
}

impl ProjectWizardConfig {
    pub fn default_for_dir(output_dir: &Path) -> Self {
        Self {
            name: "my_flutter_app".to_string(),
            org: "com.example".to_string(),
            template: "app".to_string(),
            platforms: vec![
                "android".into(),
                "ios".into(),
                "macos".into(),
                "windows".into(),
                "linux".into(),
                "web".into(),
            ],
            description: "A new Flutter project.".to_string(),
            output_dir: output_dir.to_string_lossy().into_owned(),
            empty: false,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.name.is_empty()
            || !self
                .name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
            || !self
                .name
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_lowercase())
        {
            return Err(format!(
                "Invalid project name `{}`: must be lowercase snake_case starting with a letter",
                self.name
            ));
        }

        if !ALLOWED_TEMPLATES.contains(&self.template.as_str()) {
            return Err(format!(
                "Unsupported template `{}`. Allowed: {}",
                self.template,
                ALLOWED_TEMPLATES.join(", ")
            ));
        }

        if self.org.trim().is_empty()
            || !self
                .org
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_')
        {
            return Err(format!("Invalid organization `{}`", self.org));
        }

        for platform in &self.platforms {
            if !ALLOWED_PLATFORMS.contains(&platform.as_str()) {
                return Err(format!(
                    "Unsupported platform `{platform}`. Allowed: {}",
                    ALLOWED_PLATFORMS.join(", ")
                ));
            }
        }

        if self.output_dir.trim().is_empty() {
            return Err("`outputDir` cannot be empty".to_string());
        }

        Ok(())
    }

    /// Builds strictly whitelisted arguments for `flutter create`.
    pub fn build_flutter_create_args(&self) -> Result<(PathBuf, Vec<String>), String> {
        self.validate()?;

        let parent = PathBuf::from(&self.output_dir);
        let target_dir = parent.join(&self.name);

        let mut args = vec![
            "create".to_string(),
            "--project-name".to_string(),
            self.name.clone(),
            "--org".to_string(),
            self.org.clone(),
            format!("--template={}", self.template),
            "--description".to_string(),
            self.description.clone(),
        ];

        if matches!(self.template.as_str(), "app" | "plugin" | "plugin_ffi")
            && !self.platforms.is_empty()
        {
            args.push(format!("--platforms={}", self.platforms.join(",")));
        }

        if self.empty && self.template == "app" {
            args.push("--empty".to_string());
        }

        args.push(target_dir.to_string_lossy().into_owned());
        Ok((target_dir, args))
    }

    pub fn next_template(&self) -> &'static str {
        let idx = ALLOWED_TEMPLATES
            .iter()
            .position(|&t| t == self.template)
            .unwrap_or(0);
        ALLOWED_TEMPLATES[(idx + 1) % ALLOWED_TEMPLATES.len()]
    }
}

pub fn create_wizard_file(default_output_dir: &Path) -> std::io::Result<PathBuf> {
    let dir = std::env::temp_dir().join("flutter-debugger-wizard");
    fs::create_dir_all(&dir)?;
    let path = dir.join(WIZARD_FILENAME);
    let config = ProjectWizardConfig::default_for_dir(default_output_dir);
    let content = serde_json::to_string_pretty(&config)?;
    fs::write(&path, content)?;
    Ok(path)
}

pub fn build_wizard_code_lenses(uri: &Uri, config: Option<&ProjectWizardConfig>) -> Vec<CodeLens> {
    let top_range = Range {
        start: Position::new(0, 0),
        end: Position::new(0, 0),
    };
    let uri_str = uri.as_str().to_string();
    let template_label = config
        .map(|c| format!("◎ Template: {}", c.template))
        .unwrap_or_else(|| "◎ Template: app".to_string());

    vec![
        CodeLens {
            range: top_range,
            command: Some(Command {
                title: "▷ Create Flutter Project".to_string(),
                command: "flutterDebugger.executeCreateProject".to_string(),
                arguments: Some(vec![json!(uri_str)]),
            }),
            data: None,
        },
        CodeLens {
            range: top_range,
            command: Some(Command {
                title: template_label,
                command: "flutterDebugger.cycleWizardTemplate".to_string(),
                arguments: Some(vec![json!(uri_str)]),
            }),
            data: None,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_valid_flutter_create_args() {
        let cfg = ProjectWizardConfig {
            name: "demo_app".into(),
            org: "com.acme".into(),
            template: "plugin".into(),
            platforms: vec!["android".into(), "ios".into(), "macos".into()],
            description: "Demo plugin".into(),
            output_dir: "/tmp".into(),
            empty: false,
        };
        let (target, args) = cfg.build_flutter_create_args().expect("valid");
        assert_eq!(target, PathBuf::from("/tmp/demo_app"));
        assert!(args.contains(&"--template=plugin".to_string()));
        assert!(args.contains(&"--platforms=android,ios,macos".to_string()));
    }

    #[test]
    fn rejects_invalid_project_name_and_template() {
        let mut cfg = ProjectWizardConfig::default_for_dir(Path::new("/tmp"));
        cfg.name = "Bad-Name".into();
        assert!(cfg.validate().is_err());

        cfg.name = "good_name".into();
        cfg.template = "unknown".into();
        assert!(cfg.validate().is_err());
    }
}
