use crate::protocol::DevToolsPage;

#[derive(Debug, Clone, Default)]
pub struct CliLaunchArgs {
    pub project: Option<String>,
    pub session_id: Option<String>,
    pub vm_uri: Option<String>,
    pub page: Option<DevToolsPage>,
}

impl CliLaunchArgs {
    pub fn parse(args: &[String]) -> Self {
        let mut out = Self::default();
        let mut iter = args.iter();
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--project" | "-p" => {
                    out.project = iter.next().cloned();
                }
                "--session" | "-s" => {
                    out.session_id = iter.next().cloned();
                }
                "--vm-uri" | "--uri" => {
                    out.vm_uri = iter.next().cloned();
                }
                "--page" | "--tab" => {
                    if let Some(val) = iter.next() {
                        out.page = DevToolsPage::parse(val);
                    }
                }
                other => {
                    if let Some(v) = other.strip_prefix("--project=") {
                        out.project = Some(v.to_string());
                    } else if let Some(v) = other.strip_prefix("--session=") {
                        out.session_id = Some(v.to_string());
                    } else if let Some(v) = other.strip_prefix("--vm-uri=") {
                        out.vm_uri = Some(v.to_string());
                    } else if let Some(v) = other.strip_prefix("--page=") {
                        out.page = DevToolsPage::parse(v);
                    }
                }
            }
        }
        out
    }
}

pub fn print_usage() {
    println!(
        "flutter-devtools-gpui {}
Native GPUI Flutter DevTools companion application for Zed

USAGE:
  flutter-devtools-gpui [OPTIONS]

OPTIONS:
  --project <dir>       Attach to active Flutter session for project directory
  --session <id>        Attach to a specific session ID
  --vm-uri <ws://...>   Directly connect to a Dart VM Service URI
  --page <tab>          Initial tab: inspector | network | logging | performance | memory
  -h, --help            Print help
  -V, --version         Print version",
        env!("CARGO_PKG_VERSION")
    );
}
