use gpui_kit::{Div, Styled as _, div, px};

use crate::theme::DevToolsTheme;

pub fn subtoolbar_container() -> Div {
    div()
        .w_full()
        .h(px(36.0))
        .px_3()
        .bg(DevToolsTheme::bg_panel())
        .border_b_1()
        .border_color(DevToolsTheme::border())
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
}
