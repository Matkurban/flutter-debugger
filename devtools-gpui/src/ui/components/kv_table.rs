use std::hash::{DefaultHasher, Hash, Hasher};

use gpui_kit::{
    IntoElement, ParentElement as _, SharedString, Styled as _, base::SelectableText, div, px,
};

use crate::theme::DevToolsTheme;

pub fn kv_row(key: &str, value: &str) -> impl IntoElement + use<> {
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    value.hash(&mut hasher);
    let digest = hasher.finish();

    let key_id = SharedString::from(format!("kv-k-{digest:016x}"));
    let val_id = SharedString::from(format!("kv-v-{digest:016x}"));

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
                .child(SelectableText::new(key_id, format!("{key}:"))),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_color(DevToolsTheme::text_primary())
                .child(SelectableText::new(val_id, value.to_string())),
        )
}
