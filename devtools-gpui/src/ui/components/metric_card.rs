use gpui_kit::{
    FontWeight, IntoElement, ParentElement as _, Rgba, Styled as _, div, px,
};

use crate::theme::DevToolsTheme;

pub fn metric_card(
    title: &str,
    value: &str,
    subtitle: &str,
    accent: Rgba,
) -> impl IntoElement + use<> {
    div()
        .flex_1()
        .p_3()
        .rounded(px(6.0))
        .bg(DevToolsTheme::bg_panel())
        .border_1()
        .border_color(DevToolsTheme::border())
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_xs()
                .text_color(DevToolsTheme::text_secondary())
                .child(title.to_string()),
        )
        .child(
            div()
                .font_family(DevToolsTheme::MONO_FONT)
                .text_lg()
                .font_weight(FontWeight::BOLD)
                .text_color(accent)
                .child(value.to_string()),
        )
        .child(
            div()
                .font_family(DevToolsTheme::MONO_FONT)
                .text_xs()
                .text_color(DevToolsTheme::text_muted())
                .child(subtitle.to_string()),
        )
}
