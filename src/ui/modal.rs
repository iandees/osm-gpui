//! Reusable modal dialog chrome: backdrop, centered frame, title, body, footer.
//!
//! Used inside a caller's `Render` impl. The caller owns body/footer state and
//! handles Esc / focus cycling.

use gpui::{
    div, px, rgb, rgba, AnyElement, IntoElement, ParentElement, SharedString, Styled,
};

pub struct Modal {
    title: SharedString,
    body: AnyElement,
    footer: AnyElement,
}

impl Modal {
    pub fn new(
        title: impl Into<SharedString>,
        body: impl IntoElement,
        footer: impl IntoElement,
    ) -> Self {
        Self {
            title: title.into(),
            body: body.into_any_element(),
            footer: footer.into_any_element(),
        }
    }
}

impl IntoElement for Modal {
    type Element = gpui::Div;

    fn into_element(self) -> Self::Element {
        let title = self.title;
        let body = self.body;
        let footer = self.footer;

        let frame = div()
            .w(px(420.0))
            .bg(rgb(0x0f172a))
            .border_1()
            .border_color(rgb(0x374151))
            .rounded_md()
            .shadow_lg()
            .flex()
            .flex_col()
            .child(
                div()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(rgb(0x374151))
                    .text_color(rgb(0xffffff))
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(title),
            )
            .child(div().p_4().flex().flex_col().gap_3().child(body))
            .child(
                div()
                    .px_4()
                    .py_3()
                    .border_t_1()
                    .border_color(rgb(0x374151))
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap_2()
                    .child(footer),
            );

        div()
            .absolute()
            .inset_0()
            .bg(rgba(0x00000099))
            .flex()
            .justify_center()
            .items_center()
            .child(frame)
    }
}
