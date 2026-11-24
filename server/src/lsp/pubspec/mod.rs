pub mod cache;
pub mod eval;
pub mod features;
pub mod lockfile;
pub mod parser;
pub mod registry;
pub mod version;

pub use cache::PubPackageCache;
pub use eval::DependencyEvaluation;
pub use features::{
    build_asset_hover_markdown, build_hosted_hover_markdown, build_local_path_hover_markdown,
    build_pubspec_code_actions, build_pubspec_code_lenses, build_pubspec_diagnostics,
    build_pubspec_document_links, build_pubspec_inlay_hints, build_sdk_or_git_hover_markdown,
    build_version_completion_items,
};
pub use lockfile::load_sibling_lockfile;
pub use parser::{
    DependencySection, DependencySource, ParsedDependency, ParsedPubspec, completion_context_at,
    parse_pubspec,
};
pub use registry::{ChangelogEntry, PackageInfo, PubRegistryClient};
pub use version::VersionConstraint;
