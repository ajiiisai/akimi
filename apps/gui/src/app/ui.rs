use akimi_model::NodeKind;
use gpui_kit::component::{
    button::{Button, ButtonVariants},
    ActiveTheme, Icon, IconName, Sizable,
};
use gpui_kit::gpui;
use gpui_kit::{div, prelude::*, px, App, Div, Hsla, Window};

const SHARE_COLORS: [u32; 8] = [
    0x5b6ee1, 0x1fa5a0, 0xe0607e, 0xd9913c, 0x9366d6, 0x3d9be0, 0x7ba83f, 0xde6a4b,
];

pub(crate) const HEADER_HEIGHT: f32 = 30.0;
pub(crate) const ROW_HEIGHT: f32 = 28.0;
pub(crate) const SHARE_WIDTH: f32 = 112.0;
pub(crate) const PERCENT_WIDTH: f32 = 50.0;
pub(crate) const SIZE_WIDTH: f32 = 82.0;
pub(crate) const ITEMS_WIDTH: f32 = 76.0;
pub(crate) const FILES_WIDTH: f32 = 76.0;
pub(crate) const FOLDERS_WIDTH: f32 = 76.0;
pub(crate) const MODIFIED_WIDTH: f32 = 76.0;

pub(crate) fn share_color(depth: usize) -> u32 {
    SHARE_COLORS[depth % SHARE_COLORS.len()]
}

pub(crate) fn app_mark(cx: &App) -> Div {
    div()
        .flex_none()
        .size_8()
        .flex()
        .items_center()
        .justify_center()
        .rounded(cx.theme().radius)
        .border_1()
        .border_color(cx.theme().primary.opacity(0.22))
        .bg(cx.theme().primary.opacity(0.12))
        .child(
            Icon::new(IconName::ChartPie)
                .small()
                .text_color(cx.theme().primary),
        )
}

pub(crate) fn toolbar_button(
    label: &'static str,
    id: &'static str,
    icon: IconName,
    primary: bool,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> Button {
    let button = Button::new(id)
        .small()
        .compact()
        .icon(icon)
        .label(label)
        .tooltip(label)
        .on_click(on_click);
    if primary {
        button.primary()
    } else {
        button.ghost()
    }
}

pub(crate) fn table_header(cx: &App) -> Div {
    let heading = |label: &'static str, width: f32| {
        div()
            .w(px(width))
            .flex_none()
            .px_2()
            .text_right()
            .child(label)
    };

    div()
        .flex_none()
        .h(px(HEADER_HEIGHT))
        .flex()
        .items_center()
        .border_b_1()
        .border_color(cx.theme().table_row_border)
        .bg(cx.theme().table_head)
        .text_xs()
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(cx.theme().table_head_foreground)
        .child(div().min_w_0().flex_1().pl(px(10.0)).child("Name"))
        .child(
            div()
                .w(px(SHARE_WIDTH))
                .flex_none()
                .px_2()
                .child("Share of parent"),
        )
        .child(heading("%", PERCENT_WIDTH))
        .child(heading("Size", SIZE_WIDTH))
        .child(heading("Items", ITEMS_WIDTH))
        .child(heading("Files", FILES_WIDTH))
        .child(heading("Folders", FOLDERS_WIDTH))
        .child(heading("Modified", MODIFIED_WIDTH))
}

pub(crate) fn number_cell(value: String, width: f32, color: Hsla, cx: &App) -> Div {
    div()
        .w(px(width))
        .flex_none()
        .px_2()
        .truncate()
        .font_family(cx.theme().mono_font_family.clone())
        .text_xs()
        .text_right()
        .text_color(color)
        .child(value)
}

pub(crate) fn node_icon(kind: NodeKind, expanded: bool, cx: &App) -> Icon {
    let icon = match kind {
        NodeKind::Directory if expanded => IconName::FolderOpen,
        NodeKind::Directory => IconName::FolderClosed,
        NodeKind::File => IconName::File,
        NodeKind::Symlink => IconName::ExternalLink,
        NodeKind::Other => IconName::FileText,
    };
    let color = match kind {
        NodeKind::Directory => cx.theme().warning.opacity(0.88),
        NodeKind::Symlink => cx.theme().primary,
        NodeKind::File | NodeKind::Other => cx.theme().muted_foreground,
    };
    Icon::new(icon).xsmall().text_color(color)
}
