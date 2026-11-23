use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::overlay::ServiceOverlay;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ExceptionPauseMode {
    #[default]
    None,
    Unhandled,
    All,
}

impl ExceptionPauseMode {
    pub fn next(self) -> Self {
        match self {
            Self::None => Self::Unhandled,
            Self::Unhandled => Self::All,
            Self::All => Self::None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::None => "Exc: Off",
            Self::Unhandled => "Exc: Uncaught",
            Self::All => "Exc: All",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::None => "Off (Do not pause on exceptions)",
            Self::Unhandled => "Uncaught Exceptions",
            Self::All => "All Exceptions (Caught & Uncaught)",
        }
    }

    pub fn vm_service_mode(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Unhandled => "Unhandled",
            Self::All => "All",
        }
    }

    pub fn dap_filters(self) -> Vec<&'static str> {
        match self {
            Self::None => Vec::new(),
            Self::Unhandled => vec!["Unhandled"],
            Self::All => vec!["All"],
        }
    }

    pub fn from_dap_filters(filters: &[String]) -> Self {
        if filters.iter().any(|f| f.eq_ignore_ascii_case("All")) {
            Self::All
        } else if filters.iter().any(|f| f.eq_ignore_ascii_case("Unhandled")) {
            Self::Unhandled
        } else {
            Self::None
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum DebugLibraryScope {
    #[default]
    MyCodeOnly,
    MyCodeAndPackages,
    AllCode,
}

impl DebugLibraryScope {
    pub fn next(self) -> Self {
        match self {
            Self::MyCodeOnly => Self::MyCodeAndPackages,
            Self::MyCodeAndPackages => Self::AllCode,
            Self::AllCode => Self::MyCodeOnly,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::MyCodeOnly => "My Code",
            Self::MyCodeAndPackages => "+Pkg",
            Self::AllCode => "+SDK",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::MyCodeOnly => "Just My Code (skip SDK & pub packages)",
            Self::MyCodeAndPackages => "My Code + External Pub Packages",
            Self::AllCode => "All Code (My Code + Packages + Dart/Flutter SDK)",
        }
    }

    /// Returns `(debugSdkLibraries, debugExternalPackageLibraries)`.
    pub fn dap_flags(self) -> (bool, bool) {
        match self {
            Self::MyCodeOnly => (false, false),
            Self::MyCodeAndPackages => (false, true),
            Self::AllCode => (true, true),
        }
    }

    pub fn from_dap_flags(debug_sdk: bool, debug_external_pkg: bool) -> Self {
        match (debug_sdk, debug_external_pkg) {
            (true, _) => Self::AllCode,
            (false, true) => Self::MyCodeAndPackages,
            (false, false) => Self::MyCodeOnly,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "camelCase")]
pub enum IpcRequest {
    HotReload,
    HotRestart,
    Stop,
    Status,
    ToggleOverlay {
        overlay: ServiceOverlay,
    },
    CallServiceExtension {
        method: String,
        params: Value,
    },
    OpenDevTools {
        #[serde(default)]
        page: Option<String>,
    },
    Screenshot {
        #[serde(default)]
        output_path: Option<String>,
    },
    SetExceptionPauseMode {
        mode: ExceptionPauseMode,
    },
    SetDebugLibraryScope {
        scope: DebugLibraryScope,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IpcResponse {
    pub ok: bool,
    pub message: String,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub device_id: Option<String>,
    #[serde(default)]
    pub vm_service_uri: Option<String>,
    #[serde(default)]
    pub devtools_uri: Option<String>,
    #[serde(default)]
    pub exception_pause_mode: Option<ExceptionPauseMode>,
    #[serde(default)]
    pub debug_library_scope: Option<DebugLibraryScope>,
}

impl IpcResponse {
    pub fn err(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            message: message.into(),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipc_request_and_response_roundtrip_with_debug_options_and_backward_compat() {
        let req_exc = IpcRequest::SetExceptionPauseMode {
            mode: ExceptionPauseMode::Unhandled,
        };
        let json_exc = serde_json::to_string(&req_exc).expect("serialize exc");
        assert!(json_exc.contains("\"setExceptionPauseMode\""));
        let parsed_exc: IpcRequest = serde_json::from_str(&json_exc).expect("deserialize exc");
        assert!(matches!(
            parsed_exc,
            IpcRequest::SetExceptionPauseMode {
                mode: ExceptionPauseMode::Unhandled
            }
        ));

        let req_scope = IpcRequest::SetDebugLibraryScope {
            scope: DebugLibraryScope::MyCodeAndPackages,
        };
        let json_scope = serde_json::to_string(&req_scope).expect("serialize scope");
        assert!(json_scope.contains("\"setDebugLibraryScope\""));

        // Legacy IpcResponse without exception_pause_mode / debug_library_scope must deserialize
        let legacy_resp = r#"{"ok":true,"message":"active"}"#;
        let parsed_resp: IpcResponse =
            serde_json::from_str(legacy_resp).expect("deserialize legacy");
        assert!(parsed_resp.ok);
        assert_eq!(parsed_resp.exception_pause_mode, None);
        assert_eq!(parsed_resp.debug_library_scope, None);
    }
}
