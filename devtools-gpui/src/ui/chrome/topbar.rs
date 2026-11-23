use flutter_debugger_protocol::{
    DebugLibraryScope, ExceptionPauseMode, ServiceOverlay, default_sessions_dir,
};
use gpui_kit::{
    Anchor, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window,
    assets::IconName,
    component::{
        Icon, Selectable as _, Sizable as _,
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
    let exc_mode = view.exception_pause_mode;
    let lib_scope = view.debug_library_scope;

    let session_label: SharedString = match view.active_session() {
        Some(s) => s.display_label().into(),
        None => "No Active Session".into(),
    };

    let sessions_snapshot = view.sessions.clone();
    let active_session_id = view.active_session_id.clone();
    let overlays_snapshot = view.overlays.clone();
    let entity = cx.entity().downgrade();
    let entity_for_overlays = cx.entity().downgrade();
    let entity_for_exc = cx.entity().downgrade();
    let entity_for_scope = cx.entity().downgrade();

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
                        .child(Icon::new(page.icon_name()).xsmall())
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
                                                Icon::new(IconName::Check)
                                                    .xsmall()
                                                    .text_color(DevToolsTheme::success_green()),
                                            )
                                        })
                                }))
                        }),
                )
                .child(
                    Button::new("btn-hot-reload")
                        .small()
                        .compact()
                        .icon(Icon::new(IconName::Zap))
                        .label("Reload")
                        .tooltip("Hot Reload (⌘R)")
                        .on_click(cx.listener(|this, _, _, _| {
                            this.send_cmd(BackendCommand::HotReload);
                        })),
                )
                .child(
                    Button::new("btn-hot-restart")
                        .small()
                        .compact()
                        .icon(Icon::new(IconName::RotateCw))
                        .label("Restart")
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
                        .icon(Icon::new(IconName::Crosshair))
                        .label("Inspect")
                        .tooltip("Toggle Select Widget Mode on device (ext.flutter.inspector.show)")
                        .on_click(cx.listener(|this, _, _, _| {
                            this.toggle_overlay(ServiceOverlay::Inspector);
                        })),
                )
                .child(
                    Popover::new("exc-pause-mode-popover")
                        .anchor(Anchor::TopRight)
                        .trigger(
                            Button::new("btn-exc-pause-mode")
                                .small()
                                .compact()
                                .dropdown_caret(true)
                                .selected(exc_mode != ExceptionPauseMode::None)
                                .icon(Icon::new(
                                    if exc_mode == ExceptionPauseMode::None {
                                        IconName::Shield
                                    } else {
                                        IconName::ShieldAlert
                                    },
                                ))
                                .label(exc_mode.label())
                                .tooltip("Configure Dart Exception Breakpoints (Off / Uncaught / All)"),
                        )
                        .content(move |_, _, _| {
                            let entity_exc = entity_for_exc.clone();
                            let current_exc = exc_mode;
                            let choices = [
                                (
                                    ExceptionPauseMode::None,
                                    DevToolsTheme::text_secondary(),
                                ),
                                (
                                    ExceptionPauseMode::Unhandled,
                                    DevToolsTheme::warning_yellow(),
                                ),
                                (
                                    ExceptionPauseMode::All,
                                    DevToolsTheme::error_red(),
                                ),
                            ];

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
                                        .child("Exception Breakpoints"),
                                )
                                .children(choices.into_iter().map(|(mode, color)| {
                                    let is_active = mode == current_exc;
                                    let entity_click = entity_exc.clone();
                                    div()
                                        .id(SharedString::from(format!(
                                            "exc-mode-item-{}",
                                            mode.vm_service_mode()
                                        )))
                                        .px_2()
                                        .py_1p5()
                                        .rounded(px(4.0))
                                        .cursor_pointer()
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .when(is_active, |el| el.bg(DevToolsTheme::bg_selected()))
                                        .when(!is_active, |el| {
                                            el.hover(|h| h.bg(DevToolsTheme::bg_hover()))
                                        })
                                        .on_click(move |_, _, cx| {
                                            let _ = entity_click.update(cx, |this, cx| {
                                                this.exception_pause_mode = mode;
                                                this.send_cmd(
                                                    BackendCommand::SetExceptionPauseMode(mode),
                                                );
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
                                                        .text_color(color)
                                                        .child(mode.label()),
                                                )
                                                .child(
                                                    div()
                                                        .text_xs()
                                                        .text_color(DevToolsTheme::text_secondary())
                                                        .child(mode.description()),
                                                ),
                                        )
                                        .when(is_active, |el| {
                                            el.child(
                                                Icon::new(IconName::Check)
                                                    .xsmall()
                                                    .text_color(DevToolsTheme::success_green()),
                                            )
                                        })
                                }))
                        }),
                )
                .child(
                    Popover::new("debug-scope-popover")
                        .anchor(Anchor::TopRight)
                        .trigger(
                            Button::new("btn-debug-scope")
                                .small()
                                .compact()
                                .dropdown_caret(true)
                                .selected(lib_scope != DebugLibraryScope::MyCodeOnly)
                                .icon(Icon::new(
                                    if lib_scope == DebugLibraryScope::MyCodeOnly {
                                        IconName::Code
                                    } else {
                                        IconName::Bug
                                    },
                                ))
                                .label(lib_scope.label())
                                .tooltip("Configure Debug Library Stepping Scope (My Code / +Packages / +SDK)"),
                        )
                        .content(move |_, _, _| {
                            let entity_sc = entity_for_scope.clone();
                            let current_sc = lib_scope;
                            let scopes = [
                                DebugLibraryScope::MyCodeOnly,
                                DebugLibraryScope::MyCodeAndPackages,
                                DebugLibraryScope::AllCode,
                            ];

                            div()
                                .w(px(280.0))
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
                                        .child("Debug Library Scope"),
                                )
                                .children(scopes.into_iter().map(|scope| {
                                    let is_active = scope == current_sc;
                                    let entity_click = entity_sc.clone();
                                    div()
                                        .id(SharedString::from(format!(
                                            "debug-scope-item-{}",
                                            scope.label()
                                        )))
                                        .px_2()
                                        .py_1p5()
                                        .rounded(px(4.0))
                                        .cursor_pointer()
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .when(is_active, |el| el.bg(DevToolsTheme::bg_selected()))
                                        .when(!is_active, |el| {
                                            el.hover(|h| h.bg(DevToolsTheme::bg_hover()))
                                        })
                                        .on_click(move |_, _, cx| {
                                            let _ = entity_click.update(cx, |this, cx| {
                                                this.debug_library_scope = scope;
                                                this.send_cmd(
                                                    BackendCommand::SetDebugLibraryScope(scope),
                                                );
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
                                                        .child(scope.label()),
                                                )
                                                .child(
                                                    div()
                                                        .text_xs()
                                                        .text_color(DevToolsTheme::text_secondary())
                                                        .child(scope.description()),
                                                ),
                                        )
                                        .when(is_active, |el| {
                                            el.child(
                                                Icon::new(IconName::Check)
                                                    .xsmall()
                                                    .text_color(DevToolsTheme::success_green()),
                                            )
                                        })
                                }))
                        }),
                )
                .child(
                    Popover::new("overlays-menu-popover")
                        .anchor(Anchor::TopRight)
                        .trigger(
                            Button::new("btn-overlays-menu")
                                .small()
                                .compact()
                                .dropdown_caret(true)
                                .icon(Icon::new(IconName::Layers))
                                .label("Overlays")
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
                        .icon(Icon::new(IconName::Camera))
                        .tooltip("Capture Screenshot from active Flutter session")
                        .on_click(cx.listener(|this, _, _, _| {
                            this.send_cmd(BackendCommand::TakeScreenshot);
                        })),
                )
                .child(
                    Button::new("btn-web-devtools")
                        .small()
                        .compact()
                        .icon(Icon::new(IconName::Globe))
                        .label("Web")
                        .tooltip("Open official Web DevTools in browser (for CPU Flame Chart)")
                        .on_click(cx.listener(|this, _, _, _| {
                            this.send_cmd(BackendCommand::OpenWebDevTools {
                                page: Some(this.active_page.as_slug().to_string()),
                            });
                        })),
                ),
        )
}
