use async_channel::{Receiver, Sender};
use gpui_kit::{
    AppContext as _, ClipboardItem, Context, FocusHandle, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, Window, actions,
    component::{
        WindowExt as _,
        input::{InputEvent, InputState},
        notification::NotificationType,
    },
    div,
    prelude::FluentBuilder as _,
    px,
};

use crate::backend::{
    ActiveSessionRecord, BackendCommand, BackendEvent, ConnectionStatus, DevToolsPage,
    OverlayState, ServiceOverlay, SourceLocation,
};
use crate::single_instance::SingleInstanceFocusCommand;
use crate::theme::DevToolsTheme;
use crate::ui::{
    inspector_tab::InspectorTabState, logging_tab::LoggingTabState, memory_tab::MemoryTabState,
    network_tab::NetworkTabState, performance_tab::PerformanceTabState,
    statusbar::render_statusbar, topbar::render_topbar,
};

actions!(
    flutter_devtools,
    [
        SwitchTabInspector,
        SwitchTabNetwork,
        SwitchTabLogging,
        SwitchTabPerformance,
        SwitchTabMemory,
        HotReloadAction,
        HotRestartAction,
    ]
);

pub struct RootView {
    pub active_page: DevToolsPage,
    pub cmd_tx: Sender<BackendCommand>,
    pub sessions: Vec<ActiveSessionRecord>,
    pub active_session_id: Option<String>,
    pub connection: ConnectionStatus,
    pub overlays: OverlayState,
    pub toast_banner: Option<(String, bool)>,
    pub focus_handle: FocusHandle,
    pub network: NetworkTabState,
    pub inspector: InspectorTabState,
    pub logging: LoggingTabState,
    pub performance: PerformanceTabState,
    pub memory: MemoryTabState,
}

impl RootView {
    pub fn new(
        initial_page: DevToolsPage,
        cmd_tx: Sender<BackendCommand>,
        event_rx: Receiver<BackendEvent>,
        single_instance_rx: Receiver<SingleInstanceFocusCommand>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let net_search = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Filter URL, method, or status...")
        });
        let insp_search =
            cx.new(|cx| InputState::new(window, cx).placeholder("Filter widgets by name or key..."));
        let log_search =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search logs or stack traces..."));

        cx.subscribe(&net_search, |_, _, _: &InputEvent, cx| {
            cx.notify();
        })
        .detach();
        cx.subscribe(&insp_search, |_, _, _: &InputEvent, cx| {
            cx.notify();
        })
        .detach();
        cx.subscribe(&log_search, |_, _, _: &InputEvent, cx| {
            cx.notify();
        })
        .detach();

        // Background event stream consumer
        cx.spawn_in(window, async move |this, cx| {
            while let Ok(ev) = event_rx.recv().await {
                let cont = this.update_in(cx, |this, window, cx| {
                    this.apply_backend_event(ev, window, cx);
                    cx.notify();
                });
                if cont.is_err() {
                    break;
                }
            }
        })
        .detach();

        // Single-instance wakeup & tab/session switch consumer
        cx.spawn_in(window, async move |this, cx| {
            while let Ok(focus_cmd) = single_instance_rx.recv().await {
                let cont = this.update_in(cx, |this, window, cx| {
                    window.activate_window();
                    if let Some(ref page_str) = focus_cmd.page
                        && let Some(page) = DevToolsPage::parse(page_str)
                    {
                        this.active_page = page;
                    }
                    if let Some(ref vm_uri) = focus_cmd.vm_uri {
                        let _ = this.cmd_tx.try_send(BackendCommand::ConnectDirectVmUri {
                            vm_uri: vm_uri.clone(),
                        });
                    } else if focus_cmd.project.is_some() || focus_cmd.session_id.is_some() {
                        let _ = this.cmd_tx.try_send(BackendCommand::SetPreferredProject {
                            project: focus_cmd.project.clone(),
                            session_id: focus_cmd.session_id.clone(),
                        });
                    }
                    cx.notify();
                });
                if cont.is_err() {
                    break;
                }
            }
        })
        .detach();

        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);

        Self {
            active_page: initial_page,
            cmd_tx,
            sessions: Vec::new(),
            active_session_id: None,
            connection: ConnectionStatus::WaitingForSession,
            overlays: OverlayState::default(),
            toast_banner: None,
            focus_handle,
            network: NetworkTabState::new(net_search),
            inspector: InspectorTabState::new(insp_search),
            logging: LoggingTabState::new(log_search),
            performance: PerformanceTabState::new(),
            memory: MemoryTabState::new(),
        }
    }

    pub fn active_session(&self) -> Option<&ActiveSessionRecord> {
        let id = self.active_session_id.as_deref()?;
        self.sessions.iter().find(|s| s.session_id == id)
    }

    pub fn send_cmd(&self, cmd: BackendCommand) {
        let _ = self.cmd_tx.try_send(cmd);
    }

    pub fn show_toast(&mut self, message: impl Into<String>, is_error: bool, window: &mut Window, cx: &mut Context<Self>) {
        let msg = message.into();
        self.toast_banner = Some((msg.clone(), is_error));
        let kind = if is_error {
            NotificationType::Error
        } else {
            NotificationType::Success
        };
        window.push_notification((kind, SharedString::from(msg)), cx);
        cx.notify();
    }

    pub fn copy_to_clipboard(
        &mut self,
        text: String,
        toast_label: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.show_toast(toast_label.to_string(), false, window, cx);
    }

    pub fn jump_to_zed(&self, loc: SourceLocation) {
        self.send_cmd(BackendCommand::OpenInZed(loc));
    }

    fn apply_backend_event(
        &mut self,
        ev: BackendEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match ev {
            BackendEvent::SessionsUpdated {
                sessions,
                active_session_id,
            } => {
                self.sessions = sessions;
                self.active_session_id = active_session_id;
            }
            BackendEvent::ConnectionChanged(status) => {
                self.connection = status;
            }
            BackendEvent::Toast { message, is_error } => {
                self.toast_banner = Some((message.clone(), is_error));
                let kind = if is_error {
                    NotificationType::Error
                } else {
                    NotificationType::Info
                };
                window.push_notification((kind, SharedString::from(message)), cx);
            }
            BackendEvent::OverlayStateUpdated(overlays) => {
                self.overlays = overlays;
            }
            BackendEvent::HttpRequestsBatch(batch) => {
                let newly_selected = self.network.merge_batch(batch);
                if let Some(id) = newly_selected {
                    self.send_cmd(BackendCommand::FetchHttpRequestDetail { request_id: id });
                }
            }
            BackendEvent::HttpRequestDetailLoaded { request_id, detail } => {
                self.network.apply_detail(&request_id, detail);
            }
            BackendEvent::WidgetTreeLoaded(root) => {
                let first_id = self.inspector.set_root_tree(root);
                if let Some(id) = first_id {
                    self.send_cmd(BackendCommand::SelectWidgetNode { value_id: id });
                }
            }
            BackendEvent::WidgetPropertiesLoaded {
                value_id,
                properties,
            } => {
                self.inspector.apply_properties(&value_id, properties);
            }
            BackendEvent::LogAppended(entry) => {
                self.logging.push_entry(entry);
            }
            BackendEvent::FrameAppended(frame) => {
                self.performance.push_frame(frame);
            }
            BackendEvent::MemoryAppended(snap) => {
                self.memory.push_snapshot(snap);
            }
        }
    }

    pub fn switch_tab(&mut self, page: DevToolsPage, cx: &mut Context<Self>) {
        self.active_page = page;
        if page == DevToolsPage::Inspector && self.inspector.root.is_none() {
            self.send_cmd(BackendCommand::RefreshWidgetTree);
        }
        cx.notify();
    }

    pub fn toggle_overlay(&mut self, overlay: ServiceOverlay) {
        self.send_cmd(BackendCommand::ToggleOverlay(overlay));
    }
}

impl Render for RootView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let is_waiting_with_no_snapshot = matches!(
            self.connection,
            ConnectionStatus::WaitingForSession
        ) && self.network.requests.is_empty()
            && self.logging.entries.is_empty()
            && self.inspector.root.is_none()
            && self.performance.frames.is_empty()
            && self.memory.snapshots.is_empty();

        let main_content = if is_waiting_with_no_snapshot {
            render_waiting_placeholder(self, cx).into_any_element()
        } else {
            match self.active_page {
                DevToolsPage::Inspector => self.render_inspector_tab(window, cx).into_any_element(),
                DevToolsPage::Network => self.render_network_tab(window, cx).into_any_element(),
                DevToolsPage::Logging => self.render_logging_tab(window, cx).into_any_element(),
                DevToolsPage::Performance => {
                    self.render_performance_tab(window, cx).into_any_element()
                }
                DevToolsPage::Memory => self.render_memory_tab(window, cx).into_any_element(),
            }
        };

        div()
            .id("devtools-root")
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_col()
            .bg(DevToolsTheme::bg_main())
            .text_color(DevToolsTheme::text_primary())
            .on_action(cx.listener(|this, _: &SwitchTabInspector, _, cx| {
                this.switch_tab(DevToolsPage::Inspector, cx);
            }))
            .on_action(cx.listener(|this, _: &SwitchTabNetwork, _, cx| {
                this.switch_tab(DevToolsPage::Network, cx);
            }))
            .on_action(cx.listener(|this, _: &SwitchTabLogging, _, cx| {
                this.switch_tab(DevToolsPage::Logging, cx);
            }))
            .on_action(cx.listener(|this, _: &SwitchTabPerformance, _, cx| {
                this.switch_tab(DevToolsPage::Performance, cx);
            }))
            .on_action(cx.listener(|this, _: &SwitchTabMemory, _, cx| {
                this.switch_tab(DevToolsPage::Memory, cx);
            }))
            .on_action(cx.listener(|this, _: &HotReloadAction, _, _| {
                this.send_cmd(BackendCommand::HotReload);
            }))
            .on_action(cx.listener(|this, _: &HotRestartAction, _, _| {
                this.send_cmd(BackendCommand::HotRestart);
            }))
            .child(render_topbar(self, window, cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_hidden()
                    .child(main_content),
            )
            .child(render_statusbar(self, cx))
    }
}

fn render_waiting_placeholder(view: &RootView, _cx: &mut Context<RootView>) -> impl IntoElement {
    let sessions_count = view.sessions.len();
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_4()
        .p_6()
        .child(
            div()
                .w(px(520.0))
                .p_6()
                .rounded(px(8.0))
                .bg(DevToolsTheme::bg_panel())
                .border_1()
                .border_color(DevToolsTheme::border())
                .flex()
                .flex_col()
                .gap_3()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .w(px(10.0))
                                .h(px(10.0))
                                .rounded(px(99.0))
                                .bg(DevToolsTheme::warning_yellow()),
                        )
                        .child(
                            div()
                                .text_base()
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .text_color(DevToolsTheme::text_primary())
                                .child("Waiting for active Flutter debug/run session..."),
                        ),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(DevToolsTheme::text_secondary())
                        .child("This window monitors ~/.cache/flutter-debugger/sessions/*.json every second and attaches automatically as soon as your Flutter app starts — no restart required."),
                )
                .child(
                    div()
                        .mt_2()
                        .p_3()
                        .rounded(px(6.0))
                        .bg(DevToolsTheme::bg_main())
                        .border_1()
                        .border_color(DevToolsTheme::border())
                        .font_family(DevToolsTheme::MONO_FONT)
                        .text_xs()
                        .text_color(DevToolsTheme::accent_blue())
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child("• In Zed Editor: press F4 (Start Debugger) or click 'Run' CodeLens above main()")
                        .child("• Or in Terminal: flutter-debugger-server ctl run"),
                )
                .when(sessions_count > 0, |this| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(DevToolsTheme::success_green())
                            .child(format!(
                                "Detected {sessions_count} session(s) starting up — waiting for Dart VM Service port..."
                            )),
                    )
                }),
        )
}
