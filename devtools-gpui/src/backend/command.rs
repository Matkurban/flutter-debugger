use flutter_debugger_protocol::{
    ActiveSessionRecord, DebugLibraryScope, ExceptionPauseMode, OverlayState, ServiceOverlay,
};

use crate::protocol::{
    FrameTiming, HttpRequestDetail, HttpRequestRecord, LayoutExplorerNode, LogEntry,
    MemorySnapshot, SourceLocation, WebSocketConnectionDetail, WidgetProperty, WidgetTreeNode,
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
    SetShowImplementationWidgets(bool),
    RefreshWidgetTree,
    SelectWidgetNode {
        value_id: String,
        highlight_on_device: bool,
    },
    ExpandWidgetChildren {
        value_id: String,
    },
    SetFlexProperties {
        value_id: String,
        main_axis_alignment: String,
        cross_axis_alignment: String,
    },
    SetFlexFit {
        value_id: String,
        flex_fit: String,
        refresh_value_id: String,
    },
    SetFlexFactor {
        value_id: String,
        flex_factor: u32,
        refresh_value_id: String,
    },
    SetMemoryPolling(bool),
    ForceGc,
    OpenInZed(SourceLocation),
    Shutdown,
}

/// Events pushed from the background Tokio worker to the GPUI main thread.
#[allow(clippy::large_enum_variant)]
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
    WebSocketDetailLoaded {
        request_id: String,
        detail: WebSocketConnectionDetail,
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
        layout_node: Option<LayoutExplorerNode>,
        details_subtree: Option<WidgetTreeNode>,
    },
    WidgetPropertiesLoaded {
        value_id: String,
        properties: Vec<WidgetProperty>,
        layout_node: Option<LayoutExplorerNode>,
        details_subtree: Option<WidgetTreeNode>,
    },
    WidgetLayoutUpdated {
        value_id: String,
        layout_node: LayoutExplorerNode,
    },
    LogAppended(LogEntry),
    LogUpdated(LogEntry),
    FrameAppended(FrameTiming),
    MemoryAppended(MemorySnapshot),
}
