use gpui_kit::{
    FontWeight, IntoElement, ParentElement as _, Rgba, Styled as _, div, px,
};

use crate::theme::DevToolsTheme;

pub fn pill_badge(label: &str, color: Rgba, alpha: f32) -> impl IntoElement + use<> {
    div()
        .px_1p5()
        .py(px(1.0))
        .rounded(px(3.0))
        .bg(DevToolsTheme::to_hsla(color, alpha))
        .font_family(DevToolsTheme::MONO_FONT)
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(color)
        .child(label.to_string())
}

pub fn legend_swatch(color: Rgba, label: &str) -> impl IntoElement + use<> {
    div()
        .flex()
        .items_center()
        .gap_1p5()
        .child(div().w(px(10.0)).h(px(10.0)).rounded(px(2.0)).bg(color))
        .child(
            div()
                .text_color(DevToolsTheme::text_secondary())
                .child(label.to_string()),
        )
}
