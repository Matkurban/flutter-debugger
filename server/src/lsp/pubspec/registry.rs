use std::cmp::Ordering;
use std::time::Duration;

use reqwest::{Client, StatusCode, header};
use semver::Version;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::cache::now_unix;
use crate::lsp::pubspec::version::{compare_dart_versions, parse_dart_version};

const USER_AGENT: &str = concat!("flutter-debugger-server/", env!("CARGO_PKG_VERSION"));
const PUB_V2_ACCEPT: &str = "application/vnd.pub.v2+json";
const MAX_BODY_CHARS: usize = 1200;

/// Raw JSON payload returned by Hosted Pub Repository Specification V2:
/// `GET <hosted-url>/api/packages/<package>` (`Accept: application/vnd.pub.v2+json`).
#[derive(Debug, Clone, Deserialize)]
pub struct PubPackageResponse {
    pub name: String,
    #[serde(default, rename = "isDiscontinued")]
    pub is_discontinued: bool,
    #[serde(default, rename = "replacedBy")]
    pub replaced_by: Option<String>,
    #[serde(default)]
    pub latest: Option<PubVersionEntry>,
    #[serde(default)]
    pub versions: Vec<PubVersionEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PubVersionEntry {
    pub version: String,
    #[serde(default)]
    pub retracted: bool,
    #[serde(default)]
    pub published: Option<String>,
    #[serde(default)]
    pub pubspec: PubVersionPubspec,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PubVersionPubspec {
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub repository: Option<String>,
    #[serde(default)]
    pub environment: Option<PubVersionEnvironment>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PubVersionEnvironment {
    #[serde(default)]
    pub sdk: Option<String>,
}

/// Optional metadata from `GET <hosted-url>/api/packages/<package>/score`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PubPackageScore {
    #[serde(default, rename = "grantedPoints")]
    pub granted_points: Option<u32>,
    #[serde(default, rename = "maxPoints")]
    pub max_points: Option<u32>,
    #[serde(default, rename = "likeCount")]
    pub like_count: Option<u64>,
    #[serde(default, rename = "downloadCount30Days")]
    pub download_count_30_days: Option<u64>,
}

/// Compact per-version summary retained in cache for constraint, completion & SDK evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionSummary {
    pub version: String,
    pub retracted: bool,
    pub published: Option<String>,
    pub sdk_constraint: Option<String>,
}

/// Normalized, cacheable package metadata derived from pub.dev V2 APIs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageInfo {
    pub name: String,
    pub hosted_url: String,
    pub fetched_at_unix: u64,
    pub is_discontinued: bool,
    pub replaced_by: Option<String>,
    pub description: Option<String>,
    pub repository: Option<String>,
    pub homepage: Option<String>,
    pub latest_stable: Option<String>,
    pub latest_prerelease: Option<String>,
    pub versions: Vec<VersionSummary>,
    pub score: Option<PubPackageScore>,
}

impl PackageInfo {
    #[must_use]
    pub fn package_page_url(&self) -> String {
        format!("{}/packages/{}", self.hosted_url, self.name)
    }

    #[must_use]
    pub fn changelog_page_url(&self) -> String {
        format!("{}/packages/{}/changelog", self.hosted_url, self.name)
    }
}

/// Cached changelog document for a package.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedChangelog {
    pub fetched_at_unix: u64,
    pub entries: Vec<ChangelogEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChangelogEntry {
    pub version: String,
    pub heading: String,
    pub body_markdown: String,
}

#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("package `{0}` not found on {1}")]
    NotFound(String, String),
    #[error("unexpected HTTP status {0} for `{1}`")]
    UnexpectedStatus(StatusCode, String),
}

#[derive(Debug, Clone)]
pub struct PubRegistryClient {
    http: Client,
}

impl PubRegistryClient {
    /// Builds a shared HTTP/2 + gzip + rustls + system-proxy client with connection pooling.
    pub fn new(timeout_secs: u64) -> Result<Self, RegistryError> {
        let http = Client::builder()
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(timeout_secs.max(3)))
            .connect_timeout(Duration::from_secs(5))
            .pool_max_idle_per_host(16)
            .build()?;
        Ok(Self { http })
    }

    /// Fetches normalized package metadata from `<hosted_url>/api/packages/<package>`
    /// (Hosted Pub Repository Specification V2), plus best-effort `/score` metadata.
    pub async fn fetch_package_info(
        &self,
        hosted_url: &str,
        package: &str,
    ) -> Result<PackageInfo, RegistryError> {
        let base = hosted_url.trim_end_matches('/');
        let pkg_url = format!("{base}/api/packages/{package}");
        let score_url = format!("{base}/api/packages/{package}/score");

        let pkg_fut = async {
            let resp = self
                .http
                .get(&pkg_url)
                .header(header::ACCEPT, PUB_V2_ACCEPT)
                .send()
                .await?;

            match resp.status() {
                StatusCode::OK => Ok(resp.json::<PubPackageResponse>().await?),
                StatusCode::NOT_FOUND => Err(RegistryError::NotFound(
                    package.to_string(),
                    base.to_string(),
                )),
                status => Err(RegistryError::UnexpectedStatus(status, package.to_string())),
            }
        };

        let score_fut = async {
            let resp = self.http.get(&score_url).send().await.ok()?;
            if resp.status() == StatusCode::OK {
                resp.json::<PubPackageScore>().await.ok()
            } else {
                None
            }
        };

        let (pkg_res, score) = tokio::join!(pkg_fut, score_fut);
        let pkg = pkg_res?;

        Ok(normalize_package_response(pkg, base, score))
    }

    /// Fetches and parses the package changelog from `<hosted_url>/packages/<package>/changelog`.
    pub async fn fetch_changelog(
        &self,
        hosted_url: &str,
        package: &str,
    ) -> Result<CachedChangelog, RegistryError> {
        let base = hosted_url.trim_end_matches('/');
        let url = format!("{base}/packages/{package}/changelog");

        let resp = self.http.get(&url).send().await?;
        match resp.status() {
            StatusCode::OK => {
                let html = resp.text().await?;
                let entries = parse_pubdev_changelog_html(&html);
                Ok(CachedChangelog {
                    fetched_at_unix: now_unix(),
                    entries,
                })
            }
            StatusCode::NOT_FOUND => Err(RegistryError::NotFound(
                package.to_string(),
                base.to_string(),
            )),
            status => Err(RegistryError::UnexpectedStatus(status, package.to_string())),
        }
    }
}

fn normalize_package_response(
    pkg: PubPackageResponse,
    hosted_url: &str,
    score: Option<PubPackageScore>,
) -> PackageInfo {
    let mut parsed_versions: Vec<(Version, VersionSummary)> = pkg
        .versions
        .into_iter()
        .filter_map(|v| {
            let parsed = parse_dart_version(&v.version)?;
            let summary = VersionSummary {
                version: v.version,
                retracted: v.retracted,
                published: v.published,
                sdk_constraint: v.pubspec.environment.and_then(|e| e.sdk),
            };
            Some((parsed, summary))
        })
        .collect();

    parsed_versions.sort_by(|a, b| compare_dart_versions(&b.0, &a.0));

    let latest_stable = parsed_versions
        .iter()
        .find(|(ver, s)| !s.retracted && ver.pre.is_empty())
        .map(|(_, s)| s.version.clone())
        .or_else(|| pkg.latest.as_ref().map(|l| l.version.clone()));

    let latest_prerelease = parsed_versions
        .iter()
        .find(|(ver, s)| !s.retracted && !ver.pre.is_empty())
        .map(|(_, s)| s.version.clone());

    let latest_pubspec = pkg.latest.map(|l| l.pubspec).unwrap_or_default();

    PackageInfo {
        name: pkg.name,
        hosted_url: hosted_url.to_string(),
        fetched_at_unix: now_unix(),
        is_discontinued: pkg.is_discontinued,
        replaced_by: pkg.replaced_by,
        description: latest_pubspec.description,
        repository: latest_pubspec.repository,
        homepage: latest_pubspec.homepage,
        latest_stable,
        latest_prerelease,
        versions: parsed_versions.into_iter().map(|(_, s)| s).collect(),
        score,
    }
}

/// Extracts structured version changelog entries from pub.dev's changelog HTML page.
#[must_use]
pub fn parse_pubdev_changelog_html(html: &str) -> Vec<ChangelogEntry> {
    let section = extract_changelog_section(html).unwrap_or(html);
    let mut entries = Vec::new();

    let heading_positions = find_headings(section);
    if heading_positions.is_empty() {
        return entries;
    }

    for idx in 0..heading_positions.len() {
        let (_, h_end, ref raw_heading) = heading_positions[idx];
        let body_start = h_end;
        let body_end = heading_positions
            .get(idx + 1)
            .map_or(section.len(), |next| next.0);

        let clean_heading = clean_heading_text(raw_heading);
        let Some(version) = extract_leading_version(&clean_heading) else {
            continue;
        };

        let raw_body = &section[body_start..body_end];
        let body_markdown = html_fragment_to_markdown(raw_body);

        entries.push(ChangelogEntry {
            version,
            heading: clean_heading,
            body_markdown,
        });
    }

    entries
}

/// Selects up to `max_versions` changelog entries relevant to upgrading from `current_version`
/// to `target_version`. Falls back to the latest available entry if version matching is ambiguous.
#[must_use]
pub fn select_relevant_entries<'a>(
    entries: &'a [ChangelogEntry],
    current_version: Option<&Version>,
    target_version: Option<&Version>,
    max_versions: usize,
) -> Vec<&'a ChangelogEntry> {
    if entries.is_empty() || max_versions == 0 {
        return Vec::new();
    }

    let mut selected = Vec::new();
    for entry in entries {
        if selected.len() >= max_versions {
            break;
        }
        let Some(entry_ver) = parse_dart_version(&entry.version) else {
            continue;
        };

        if let Some(target) = target_version
            && compare_dart_versions(&entry_ver, target) == Ordering::Greater
        {
            continue;
        }
        if let Some(current) = current_version
            && compare_dart_versions(&entry_ver, current) != Ordering::Greater
        {
            if !selected.is_empty() {
                break;
            }
            continue;
        }

        selected.push(entry);
    }

    if selected.is_empty() {
        entries.iter().take(max_versions.min(1)).collect()
    } else {
        selected
    }
}

fn extract_changelog_section(html: &str) -> Option<&str> {
    let marker = "detail-tab-changelog-content";
    let marker_idx = html.find(marker)?;
    let start_tag_end = html[marker_idx..].find('>')? + marker_idx + 1;
    let rest = &html[start_tag_end..];
    let end_idx = rest.find("</section>").unwrap_or(rest.len());
    Some(&rest[..end_idx])
}

fn find_headings(html: &str) -> Vec<(usize, usize, String)> {
    let mut results = Vec::new();
    let lower = html.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut pos = 0;

    while pos < bytes.len() {
        let Some(rel) = lower[pos..].find("<h") else {
            break;
        };
        let start = pos + rel;
        let Some(&level_byte) = bytes.get(start + 2) else {
            break;
        };
        if !matches!(level_byte, b'1' | b'2' | b'3') {
            pos = start + 2;
            continue;
        }
        let next_char = bytes.get(start + 3).copied().unwrap_or(b' ');
        if next_char != b'>' && !next_char.is_ascii_whitespace() {
            pos = start + 3;
            continue;
        }

        let Some(open_end_rel) = lower[start..].find('>') else {
            break;
        };
        let open_end = start + open_end_rel + 1;
        let close_tag = format!("</h{}>", level_byte as char);
        let Some(close_rel) = lower[open_end..].find(&close_tag) else {
            pos = open_end;
            continue;
        };
        let close_start = open_end + close_rel;
        let close_end = close_start + close_tag.len();

        results.push((start, close_end, html[open_end..close_start].to_string()));
        pos = close_end;
    }

    results
}

fn clean_heading_text(raw_html: &str) -> String {
    let text = strip_tags(raw_html);
    let decoded = decode_html_entities(&text);
    decoded.trim().trim_end_matches('#').trim().to_string()
}

fn extract_leading_version(heading: &str) -> Option<String> {
    for token in heading.split_whitespace() {
        let cleaned = token
            .trim_matches(|c: char| matches!(c, '[' | ']' | '(' | ')' | ':' | ','))
            .trim_start_matches(['v', 'V']);
        if parse_dart_version(cleaned).is_some() {
            return Some(cleaned.to_string());
        }
    }
    None
}

fn html_fragment_to_markdown(fragment: &str) -> String {
    let mut out = String::new();
    let mut i = 0;
    let bytes = fragment.as_bytes();
    let mut in_code = false;
    let mut in_pre = false;

    while i < bytes.len() {
        if bytes[i] == b'<'
            && let Some(close_rel) = fragment[i..].find('>')
        {
            let tag_raw = &fragment[i + 1..i + close_rel];
            let tag_lower = tag_raw.trim().to_ascii_lowercase();
            let tag_name = tag_lower
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim_end_matches('/');

            match tag_name {
                "li" => {
                    if !out.ends_with('\n') && !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str("- ");
                }
                "/li" | "/p" | "/div" | "br" => {
                    if !out.ends_with('\n') {
                        out.push('\n');
                    }
                }
                "p" => {
                    if !out.is_empty() && !out.ends_with("\n\n") {
                        if !out.ends_with('\n') {
                            out.push('\n');
                        }
                        out.push('\n');
                    }
                }
                "pre" => {
                    in_pre = true;
                    if !out.ends_with('\n') && !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str("```\n");
                }
                "/pre" => {
                    in_pre = false;
                    if !out.ends_with('\n') {
                        out.push('\n');
                    }
                    out.push_str("```\n");
                }
                "code" if !in_pre => {
                    in_code = true;
                    out.push('`');
                }
                "/code" if !in_pre && in_code => {
                    in_code = false;
                    out.push('`');
                }
                "strong" | "b" | "/strong" | "/b" => out.push_str("**"),
                "em" | "i" | "/em" | "/i" => out.push('*'),
                _ => {}
            }

            i += close_rel + 1;
            continue;
        }

        let next_tag = fragment[i..].find('<').map_or(bytes.len(), |rel| i + rel);
        let text = decode_html_entities(&fragment[i..next_tag]);
        if in_pre {
            out.push_str(&text);
        } else {
            let mut prev_ws = out.ends_with(|c: char| c.is_whitespace());
            for ch in text.chars() {
                if ch.is_whitespace() {
                    if !prev_ws && !out.is_empty() {
                        out.push(' ');
                        prev_ws = true;
                    }
                } else {
                    out.push(ch);
                    prev_ws = false;
                }
            }
        }
        i = next_tag;
    }

    let cleaned: Vec<&str> = out
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty())
        .collect();
    let joined = cleaned.join("\n");

    if joined.chars().count() > MAX_BODY_CHARS {
        let truncated: String = joined.chars().take(MAX_BODY_CHARS).collect();
        format!("{truncated}\n…")
    } else {
        joined
    }
}

fn strip_tags(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_tag = false;
    for ch in input.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out
}

fn decode_html_entities(input: &str) -> String {
    input
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
        .replace("&#x2F;", "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pubdev_changelog_html_and_selects_diff_entries() {
        let html = r##"
        <html><body>
          <section class="tab-content detail-tab-changelog-content">
            <h2 class="hash-header" id="200">2.0.0 <a class="hash-link" href="#200">#</a></h2>
            <ul>
              <li><strong>BREAKING</strong>: Dropped legacy API.</li>
              <li>Added <code>Client.withTimeout()</code>.</li>
            </ul>
            <h2 class="hash-header" id="120">1.2.0 <a class="hash-link" href="#120">#</a></h2>
            <ul>
              <li>Improved HTTP/2 pooling &amp; retry logic.</li>
            </ul>
            <h2 class="hash-header" id="110">1.1.0 <a class="hash-link" href="#110">#</a></h2>
            <ul>
              <li>Initial release.</li>
            </ul>
          </section>
        </body></html>
        "##;

        let entries = parse_pubdev_changelog_html(html);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].version, "2.0.0");
        assert!(entries[0].body_markdown.contains("**BREAKING**"));
        assert!(entries[0].body_markdown.contains("`Client.withTimeout()`"));
        assert!(entries[1].body_markdown.contains("pooling & retry"));

        let cur = parse_dart_version("1.1.0").expect("cur");
        let latest = parse_dart_version("2.0.0").expect("latest");
        let relevant = select_relevant_entries(&entries, Some(&cur), Some(&latest), 3);
        assert_eq!(relevant.len(), 2);
        assert_eq!(relevant[0].version, "2.0.0");
        assert_eq!(relevant[1].version, "1.2.0");
    }
}
