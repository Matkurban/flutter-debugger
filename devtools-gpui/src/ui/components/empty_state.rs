use gpui_kit::{IntoElement, ParentElement as _, Styled as _, div};

use crate::theme::DevToolsTheme;

pub fn empty_placeholder(message: &str) -> impl IntoElement + use<> {
    div()
        .size_full()
        .bg(DevToolsTheme::bg_panel())
        .flex()
        .items_center()
        .justify_center()
        .text_sm()
        .text_color(DevToolsTheme::text_muted())
        .child(message.to_string())
}
