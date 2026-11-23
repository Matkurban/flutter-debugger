use gpui_kit::{
    Context, IntoElement, ParentElement as _, Styled as _, div, prelude::FluentBuilder as _, px,
};

use crate::backend::{ConnectionStatus, DevToolsPage};
use crate::theme::{DevToolsTheme, format_bytes};
use crate::ui::app::RootView;

pub fn render_statusbar(view: &RootView, _cx: &mut Context<RootView>) -> impl IntoElement {
    let (dot_color, conn_text) = match &view.connection {
        ConnectionStatus::WaitingForSession => (
            DevToolsTheme::warning_yellow(),
            "Waiting for Flutter session".to_string(),
        ),
        ConnectionStatus::Connecting(uri) => {
            (DevToolsTheme::accent_blue(), format!("Connecting {uri}..."))
        }
        ConnectionStatus::Connected {
            vm_uri,
            isolate_name,
            target_cpu,
            arch_bits,
        } => (
            DevToolsTheme::success_green(),
            format!("Connected · {isolate_name} ({target_cpu} {arch_bits}-bit) · {vm_uri}"),
        ),
        ConnectionStatus::Disconnected(reason) => (
            DevToolsTheme::error_red(),
            format!("Disconnected ({reason}) — snapshot retained"),
        ),
    };

    let tab_summary = match view.active_page {
        DevToolsPage::Network => format!(
            "Network: {} requests ({})",
            view.network.requests.len(),
            if view.network.recording {
                "Recording"
            } else {
                "Paused"
            }
        ),
        DevToolsPage::Inspector => {
            let count = view.inspector.flattened_count();
            format!("Inspector: {count} widget nodes")
        }
        DevToolsPage::Logging => {
            let err_count = view
                .logging
                .entries
                .iter()
                .filter(|e| e.kind.is_error())
                .count();
            format!(
                "Logs: {} entries ({err_count} errors)",
                view.logging.entries.len()
            )
        }
        DevToolsPage::Performance => format!(
            "Frames: {} captured ({} jank)",
            view.performance.frames.len(),
            view.performance.jank_count()
        ),
        DevToolsPage::Memory => {
            if let Some(latest) = view.memory.snapshots.back() {
                format!(
                    "Heap: {} / {} (RSS {})",
                    format_bytes(latest.heap_usage_bytes),
                    format_bytes(latest.heap_capacity_bytes),
                    format_bytes(latest.rss_bytes)
                )
            } else {
                "Memory: waiting for sample".to_string()
            }
        }
    };

    let session_meta = view.active_session().map(|s| {
        format!(
            "{} · {} · {} · PID {}",
            s.project_name(),
            s.device_id.as_deref().unwrap_or("auto"),
            s.mode,
            s.pid
        )
    });

    div()
        .w_full()
        .h(px(26.0))
        .px_3()
        .bg(DevToolsTheme::bg_toolbar())
        .border_t_1()
        .border_color(DevToolsTheme::border())
        .font_family(DevToolsTheme::MONO_FONT)
        .text_xs()
        .text_color(DevToolsTheme::text_secondary())
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .child(div().w(px(7.0)).h(px(7.0)).rounded(px(99.0)).bg(dot_color))
                        .child(
                            div()
                                .text_color(DevToolsTheme::text_primary())
                                .child(conn_text),
                        ),
                )
                .child(div().text_color(DevToolsTheme::border()).child("|"))
                .child(div().child(tab_summary)),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .when_some(
                    view.toast_banner.as_ref(),
                    |el, (toast_msg, is_error)| {
                        el.child(
                            div()
                                .text_color(if *is_error {
                                    DevToolsTheme::error_red()
                                } else {
                                    DevToolsTheme::accent_cyan()
                                })
                                .child(toast_msg.clone()),
                        )
                        .child(div().text_color(DevToolsTheme::border()).child("|"))
                    },
                )
                .when_some(session_meta, |el, meta| el.child(div().child(meta))),
        )
}
