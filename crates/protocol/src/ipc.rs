use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::overlay::ServiceOverlay;

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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
}

impl IpcResponse {
    pub fn err(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            message: message.into(),
            duration_ms: None,
            mode: None,
            device_id: None,
            vm_service_uri: None,
            devtools_uri: None,
        }
    }
}
