use flutter_debugger_protocol::{ServiceOverlay, default_sessions_dir};
use gpui_kit::{
    Anchor, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window,
    component::{
        Selectable as _, Sizable as _,
        button::Button,
        popover::Popover,
    },
    div,
    prelude::FluentBuilder as _,
    px,
};

use crate::backend::BackendCommand;
use crate::protocol::DevToolsPage;
use crate::theme::DevToolsTheme;
use crate::ui::app::RootView;

pub fn render_topbar(
    view: &mut RootView,
    _window: &mut Window,
    cx: &mut Context<RootView>,
) -> impl IntoElement {
    let active_page = view.active_page;
    let inspector_active = view.overlays.inspector;

    let session_label: SharedString = match view.active_session() {
        Some(s) => s.display_label().into(),
        None => "No Active Session".into(),
    };

    let sessions_snapshot = view.sessions.clone();
    let active_session_id = view.active_session_id.clone();
    let overlays_snapshot = view.overlays.clone();
    let entity = cx.entity().downgrade();
    let entity_for_overlays = cx.entity().downgrade();

    div()
        .w_full()
        .h(px(40.0))
        .px_3()
        .bg(DevToolsTheme::bg_toolbar())
        .border_b_1()
        .border_color(DevToolsTheme::border())
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .child(
            div()
                .h_full()
                .flex()
                .items_center()
                .gap_1()
                .child(
                    div()
                        .pr_3()
                        .mr_1()
                        .border_r_1()
                        .border_color(DevToolsTheme::border())
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .w(px(8.0))
                                .h(px(8.0))
                                .rounded(px(99.0))
                                .bg(DevToolsTheme::accent_blue()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .font_weight(gpui_kit::FontWeight::BOLD)
                                .text_color(DevToolsTheme::text_primary())
                                .child("Flutter DevTools"),
                        ),
                )
                .children(DevToolsPage::all().into_iter().map(|page| {
                    let is_active = page == active_page;
                    let badge_count = match page {
                        DevToolsPage::Network => Some(view.network.requests.len()),
                        DevToolsPage::Logging => Some(view.logging.entries.len()),
                        DevToolsPage::Performance => {
                            let jank = view.performance.jank_count();
                            if jank > 0 { Some(jank) } else { None }
                        }
                        _ => None,
                    };

                    div()
                        .id(SharedString::from(format!("tab-{}", page.as_slug())))
                        .h_full()
                        .px_3()
                        .cursor_pointer()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .border_b_2()
                        .when(is_active, |el| {
                            el.border_color(DevToolsTheme::accent_blue())
                                .bg(DevToolsTheme::bg_elevated())
                                .text_color(DevToolsTheme::text_primary())
                        })
                        .when(!is_active, |el| {
                            el.border_color(gpui_kit::transparent_black())
                                .text_color(DevToolsTheme::text_secondary())
                                .hover(|h| {
                                    h.bg(DevToolsTheme::bg_hover())
                                        .text_color(DevToolsTheme::text_primary())
                                })
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.switch_tab(page, cx);
                        }))
                        .child(
                            div()
                                .text_xs()
                                .font_weight(if is_active {
                                    gpui_kit::FontWeight::SEMIBOLD
                                } else {
                                    gpui_kit::FontWeight::NORMAL
                                })
                                .child(page.label()),
                        )
                        .when_some(badge_count.filter(|&c| c > 0), |el, count| {
                            let is_jank_badge = page == DevToolsPage::Performance;
                            el.child(
                                div()
                                    .px_1p5()
                                    .py(px(1.0))
                                    .rounded(px(99.0))
                                    .bg(if is_jank_badge {
                                        DevToolsTheme::to_hsla(DevToolsTheme::error_red(), 0.22)
                                    } else {
                                        DevToolsTheme::to_hsla(DevToolsTheme::accent_blue(), 0.2)
                                    })
                                    .font_family(DevToolsTheme::MONO_FONT)
                                    .text_xs()
                                    .text_color(if is_jank_badge {
                                        DevToolsTheme::error_red()
                                    } else {
                                        DevToolsTheme::accent_blue()
                                    })
                                    .child(format!("{count}")),
                            )
                        })
                })),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_1p5()
                .child(
                    Popover::new("session-picker-popover")
                        .anchor(Anchor::TopRight)
                        .trigger(
                            Button::new("session-selector-btn")
                                .small()
                                .compact()
                                .dropdown_caret(true)
                                .label(session_label)
                                .tooltip("Switch active Flutter debug/run session"),
                        )
                        .content(move |_, _, _| {
                            let entity = entity.clone();
                            let sessions = sessions_snapshot.clone();
                            let current_id = active_session_id.clone();

                            div()
                                .w(px(360.0))
                                .p_1p5()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .px_2()
                                        .py_1()
                                        .text_xs()
                                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                        .text_color(DevToolsTheme::text_secondary())
                                        .child("Active Flutter Sessions"),
                                )
                                .when(sessions.is_empty(), |el| {
                                    el.child(
                                        div()
                                            .px_2()
                                            .py_2()
                                            .text_xs()
                                            .text_color(DevToolsTheme::text_muted())
                                            .child(format!(
                                                "No active sessions found in {}",
                                                default_sessions_dir().display()
                                            )),
                                    )
                                })
                                .children(sessions.into_iter().map(|sess| {
                                    let is_selected =
                                        current_id.as_deref() == Some(sess.session_id.as_str());
                                    let sid = sess.session_id.clone();
                                    let entity_click = entity.clone();

                                    div()
                                        .id(SharedString::from(format!("sess-item-{sid}")))
                                        .px_2()
                                        .py_1p5()
                                        .rounded(px(4.0))
                                        .cursor_pointer()
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .when(is_selected, |el| el.bg(DevToolsTheme::bg_selected()))
                                        .when(!is_selected, |el| {
                                            el.hover(|h| h.bg(DevToolsTheme::bg_hover()))
                                        })
                                        .on_click(move |_, _, cx| {
                                            let _ = entity_click.update(cx, |this, cx| {
                                                this.send_cmd(BackendCommand::SelectSession {
                                                    session_id: sid.clone(),
                                                });
                                                cx.notify();
                                            });
                                        })
                                        .child(
                                            div()
                                                .flex()
                                                .flex_col()
                                                .child(
                                                    div()
                                                        .text_xs()
                                                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                                        .text_color(DevToolsTheme::text_primary())
                                                        .child(sess.display_label()),
                                                )
                                                .child(
                                                    div()
                                                        .font_family(DevToolsTheme::MONO_FONT)
                                                        .text_xs()
                                                        .text_color(DevToolsTheme::text_secondary())
                                                        .child(format!(
                                                            "PID {} · {}",
                                                            sess.pid,
                                                            sess.vm_service_uri
                                                                .as_deref()
                                                                .unwrap_or("connecting...")
                                                        )),
                                                ),
                                        )
                                        .when(is_selected, |el| {
                                            el.child(
                                                div()
                                                    .text_xs()
                                                    .text_color(DevToolsTheme::success_green())
                                                    .child("●"),
                                            )
                                        })
                                }))
                        }),
                )
                .child(
                    Button::new("btn-hot-reload")
                        .small()
                        .compact()
                        .label("⚡ Reload")
                        .tooltip("Hot Reload (⌘R)")
                        .on_click(cx.listener(|this, _, _, _| {
                            this.send_cmd(BackendCommand::HotReload);
                        })),
                )
                .child(
                    Button::new("btn-hot-restart")
                        .small()
                        .compact()
                        .label("🔄 Restart")
                        .tooltip("Hot Restart (⌘⇧R)")
                        .on_click(cx.listener(|this, _, _, _| {
                            this.send_cmd(BackendCommand::HotRestart);
                        })),
                )
                .child(
                    Button::new("btn-select-widget")
                        .small()
                        .compact()
                        .selected(inspector_active)
                        .label("🎯 Inspect")
                        .tooltip("Toggle Select Widget Mode on device (ext.flutter.inspector.show)")
                        .on_click(cx.listener(|this, _, _, _| {
                            this.toggle_overlay(ServiceOverlay::Inspector);
                        })),
                )
                .child(
                    Popover::new("overlays-menu-popover")
                        .anchor(Anchor::TopRight)
                        .trigger(
                            Button::new("btn-overlays-menu")
                                .small()
                                .compact()
                                .dropdown_caret(true)
                                .label("🧩 Overlays")
                                .tooltip("Toggle Flutter Service Extension Overlays"),
                        )
                        .content(move |_, _, _| {
                            let entity_ov = entity_for_overlays.clone();
                            let ov_state = overlays_snapshot.clone();

                            div()
                                .w(px(260.0))
                                .p_1p5()
                                .flex()
                                .flex_col()
                                .gap_0p5()
                                .child(
                                    div()
                                        .px_2()
                                        .py_1()
                                        .text_xs()
                                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                        .text_color(DevToolsTheme::text_secondary())
                                        .child("Flutter Service Extensions"),
                                )
                                .children(ServiceOverlay::all().into_iter().map(|overlay| {
                                    let is_on = ov_state.is_enabled(overlay);
                                    let badge = ov_state.badge_text(overlay);
                                    let entity_click = entity_ov.clone();

                                    div()
                                        .id(SharedString::from(format!(
                                            "overlay-toggle-{}",
                                            overlay.label()
                                        )))
                                        .px_2()
                                        .py_1p5()
                                        .rounded(px(4.0))
                                        .cursor_pointer()
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .hover(|h| h.bg(DevToolsTheme::bg_hover()))
                                        .on_click(move |_, _, cx| {
                                            let _ = entity_click.update(cx, |this, _| {
                                                this.toggle_overlay(overlay);
                                            });
                                        })
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(DevToolsTheme::text_primary())
                                                .child(overlay.label()),
                                        )
                                        .child(
                                            div()
                                                .px_1p5()
                                                .py(px(1.0))
                                                .rounded(px(4.0))
                                                .font_family(DevToolsTheme::MONO_FONT)
                                                .text_xs()
                                                .bg(if is_on {
                                                    DevToolsTheme::to_hsla(
                                                        DevToolsTheme::success_green(),
                                                        0.2,
                                                    )
                                                } else {
                                                    DevToolsTheme::bg_main().into()
                                                })
                                                .text_color(if is_on {
                                                    DevToolsTheme::success_green()
                                                } else {
                                                    DevToolsTheme::text_secondary()
                                                })
                                                .child(badge),
                                        )
                                }))
                        }),
                )
                .child(
                    Button::new("btn-screenshot")
                        .small()
                        .compact()
                        .label("📸")
                        .tooltip("Capture Screenshot from active Flutter session")
                        .on_click(cx.listener(|this, _, _, _| {
                            this.send_cmd(BackendCommand::TakeScreenshot);
                        })),
                )
                .child(
                    Button::new("btn-web-devtools")
                        .small()
                        .compact()
                        .label("🌐 Web")
                        .tooltip("Open official Web DevTools in browser (for CPU Flame Chart)")
                        .on_click(cx.listener(|this, _, _, _| {
                            this.send_cmd(BackendCommand::OpenWebDevTools {
                                page: Some(this.active_page.as_slug().to_string()),
                            });
                        })),
                ),
        )
}
