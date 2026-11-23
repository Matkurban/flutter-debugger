use gpui_kit::{IntoElement, ParentElement as _, Styled as _, div, px};

use crate::theme::DevToolsTheme;

pub fn kv_row(key: &str, value: &str) -> impl IntoElement + use<> {
    div()
        .w_full()
        .font_family(DevToolsTheme::MONO_FONT)
        .text_xs()
        .flex()
        .items_start()
        .gap_2()
        .child(
            div()
                .w(px(140.0))
                .flex_shrink_0()
                .text_color(DevToolsTheme::text_secondary())
                .child(format!("{key}:")),
        )
        .child(
            div()
                .flex_1()
                .text_color(DevToolsTheme::text_primary())
                .child(value.to_string()),
        )
}
