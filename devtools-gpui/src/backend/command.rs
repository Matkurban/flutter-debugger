use flutter_debugger_protocol::{
    ActiveSessionRecord, DebugLibraryScope, ExceptionPauseMode, OverlayState, ServiceOverlay,
};

use crate::protocol::{
    FrameTiming, HttpRequestDetail, HttpRequestRecord, LogEntry, MemorySnapshot, SourceLocation,
    WidgetProperty, WidgetTreeNode,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionStatus {
    WaitingForSession,
    Connecting(String),
    Connected {
        vm_uri: String,
        isolate_name: String,
        target_cpu: String,
        arch_bits: u64,
    },
    Disconnected(String),
}

/// Commands sent from the GPUI main thread to the background Tokio worker.
#[derive(Debug, Clone)]
pub enum BackendCommand {
    SelectSession {
        session_id: String,
    },
    ConnectDirectVmUri {
        vm_uri: String,
    },
    SetPreferredProject {
        project: Option<String>,
        session_id: Option<String>,
    },
    HotReload,
    HotRestart,
    ToggleOverlay(ServiceOverlay),
    SetExceptionPauseMode(ExceptionPauseMode),
    SetDebugLibraryScope(DebugLibraryScope),
    TakeScreenshot,
    OpenWebDevTools {
        page: Option<String>,
    },
    SetNetworkRecording(bool),
    ClearNetworkProfile,
    FetchHttpRequestDetail {
        request_id: String,
        isolate_id: Option<String>,
    },
    RefreshWidgetTree,
    SelectWidgetNode {
        value_id: String,
    },
    ExpandWidgetChildren {
        value_id: String,
    },
    SetMemoryPolling(bool),
    ForceGc,
    OpenInZed(SourceLocation),
    Shutdown,
}

/// Events pushed from the background Tokio worker to the GPUI main thread.
#[derive(Debug, Clone)]
pub enum BackendEvent {
    SessionsUpdated {
        sessions: Vec<ActiveSessionRecord>,
        active_session_id: Option<String>,
    },
    ConnectionChanged(ConnectionStatus),
    Toast {
        message: String,
        is_error: bool,
    },
    OverlayStateUpdated(OverlayState),
    DebugOptionsUpdated {
        exception_pause_mode: ExceptionPauseMode,
        debug_library_scope: DebugLibraryScope,
    },
    HttpRequestsBatch(Vec<HttpRequestRecord>),
    HttpRequestDetailLoaded {
        request_id: String,
        detail: HttpRequestDetail,
    },
    HttpRequestDetailFailed {
        request_id: String,
        error: String,
    },
    WidgetTreeLoaded(WidgetTreeNode),
    WidgetChildrenLoaded {
        parent_value_id: String,
        children: Vec<WidgetTreeNode>,
    },
    WidgetSelectedFromDevice {
        tree: Option<WidgetTreeNode>,
        selected_node: WidgetTreeNode,
        properties: Vec<WidgetProperty>,
    },
    WidgetPropertiesLoaded {
        value_id: String,
        properties: Vec<WidgetProperty>,
    },
    LogAppended(LogEntry),
    LogUpdated(LogEntry),
    FrameAppended(FrameTiming),
    MemoryAppended(MemorySnapshot),
}
