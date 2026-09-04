use std::cell::Cell;
use std::ffi::OsString;
use std::fs;
use std::ops::Range;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use akimi_ext4::FilesystemScan;
use akimi_model::{NodeId, NodeKind};
use gpui_kit::component::{
    alert::Alert,
    breadcrumb::{Breadcrumb, BreadcrumbItem},
    button::{Button, ButtonVariants},
    menu::{ContextMenuExt, PopupMenu, PopupMenuItem},
    notification::Notification,
    resizable_panel,
    scroll::ScrollableElement,
    spinner::Spinner,
    status_bar::StatusBar,
    tooltip::Tooltip,
    v_resizable, ActiveTheme, Icon, IconName, Root, Sizable, Theme, ThemeMode, WindowExt,
};
use gpui_kit::gpui;
use gpui_kit::{
    canvas, div, fill, linear_color_stop, linear_gradient, point, prelude::*, px, quad, relative,
    rgb, size, transparent_black, uniform_list, AnyElement, App, BorderStyle, Bounds, Context, Div,
    Hsla, MouseButton, MouseDownEvent, Pixels, Point, ScrollDelta, ScrollWheelEvent, Stateful,
    UniformListScrollHandle, WeakEntity, Window, WindowBounds, WindowOptions,
};

mod device_access;
mod file_ops;
mod formatting;
mod tree_model;
mod treemap;
mod ui;
mod volumes;

use file_ops::{DeleteMode, DeleteTarget};
use formatting::{
    count as format_count, duration as format_duration, modified_time as format_mtime,
    size as format_size,
};
use tree_model::{ancestor_chain, TreeModel};
use treemap::{build_treemap, Rect, ScrollAmount, Treemap, TreemapViewport};
use ui::{
    app_mark, node_icon, number_cell as num_cell, share_color, table_header, toolbar_button,
    FILES_WIDTH as FILES_W, FOLDERS_WIDTH as FOLDERS_W, ITEMS_WIDTH as ITEMS_W,
    MODIFIED_WIDTH as MODIFIED_W, PERCENT_WIDTH as PERCENT_W, ROW_HEIGHT as ROW_H,
    SHARE_WIDTH as SHARE_W, SIZE_WIDTH as SIZE_W,
};
use volumes::{discover as detect_volumes, Volume};

struct ReadyState {
    device: PathBuf,
    mount_point: Option<PathBuf>,
    mount_device: Option<u64>,
    scan: Arc<FilesystemScan>,
    selected: NodeId,
    tree: TreeModel,
    treemap: TreemapViewport,
    elapsed: Duration,
}

impl ReadyState {
    fn new(
        device: PathBuf,
        mount_point: Option<PathBuf>,
        mount_device: Option<u64>,
        scan: Arc<FilesystemScan>,
        map: Arc<Treemap>,
        elapsed: Duration,
    ) -> Self {
        let tree = TreeModel::new(&scan);
        Self {
            device,
            mount_point,
            mount_device,
            scan,
            selected: NodeId::ROOT,
            tree,
            treemap: TreemapViewport::new(map),
            elapsed,
        }
    }

    fn select(&mut self, id: NodeId) {
        self.selected = id;
    }

    fn toggle_directory(&mut self, id: NodeId) -> bool {
        if !self.tree.toggle(&self.scan, id) {
            return false;
        }
        self.selected = id;
        true
    }

    fn reveal(&mut self, id: NodeId) -> Option<usize> {
        self.selected = id;
        self.tree.reveal(&self.scan, id)
    }

    fn collapse_all(&mut self) {
        self.tree.collapse_all(&self.scan);
        self.selected = NodeId::ROOT;
    }
}

#[derive(Clone)]
struct DeleteSelection {
    target: DeleteTarget,
    device: PathBuf,
    name: String,
    is_directory: bool,
}

struct DeleteProgress {
    name: String,
    mode: DeleteMode,
}

enum Stage {
    Picker,
    Scanning { device: PathBuf },
    Ready(ReadyState),
    Failed { device: PathBuf, message: String },
}

struct Akimi {
    volumes: Vec<Volume>,
    selected_volume: usize,
    stage: Stage,
    map_bounds: Rc<Cell<Bounds<Pixels>>>,
    tree_scroll: UniformListScrollHandle,
    delete_confirmation: Option<DeleteSelection>,
    deletion: Option<DeleteProgress>,
}

impl Akimi {
    fn new() -> Self {
        Self {
            volumes: detect_volumes(),
            selected_volume: 0,
            stage: Stage::Picker,
            map_bounds: Rc::new(Cell::new(Bounds::default())),
            tree_scroll: UniformListScrollHandle::new(),
            delete_confirmation: None,
            deletion: None,
        }
    }

    fn start_scan(&mut self, cx: &mut Context<Self>) {
        let Some(volume) = self.volumes.get(self.selected_volume) else {
            return;
        };
        let device = volume.device.clone();
        let mount_point = volume.mount_point.clone();
        let scan_path = volume.scan_path.clone().unwrap_or_else(|| device.clone());
        self.delete_confirmation = None;
        self.stage = Stage::Scanning {
            device: device.clone(),
        };
        cx.notify();

        let workers = std::thread::available_parallelism()
            .map(|value| value.get())
            .unwrap_or(1);
        let task = cx.background_executor().spawn(async move {
            let started = Instant::now();
            let mount_device = mount_point
                .as_ref()
                .and_then(|path| fs::metadata(path).ok())
                .map(|metadata| metadata.dev());
            let mut filesystem = device_access::open_for_scan(&scan_path)?;
            let scan = match filesystem.scan_with_threads(workers) {
                Ok(scan) => scan,
                Err(error) if filesystem.is_btrfs() && error.is_permission_denied() => {
                    device_access::scan_btrfs_with_helper(&scan_path)?
                }
                Err(error) => return Err(error.to_string()),
            };
            let scan = Arc::new(scan);
            // Build the initial root layout off the UI thread. Zoomed subtree
            // layouts are built on demand and cached; window resizing only
            // rescales whichever layout is active.
            let map = Arc::new(build_treemap(&scan, NodeId::ROOT));
            Ok::<_, String>((
                device,
                mount_point,
                mount_device,
                scan,
                map,
                started.elapsed(),
            ))
        });

        cx.spawn(async move |this, cx| {
            let outcome = task.await;
            let _ = this.update(cx, |this, cx| {
                match outcome {
                    Ok((device, mount_point, mount_device, scan, map, elapsed)) => {
                        this.stage = Stage::Ready(ReadyState::new(
                            device,
                            mount_point,
                            mount_device,
                            scan,
                            map,
                            elapsed,
                        ));
                    }
                    Err(message) => {
                        let device = match &this.stage {
                            Stage::Scanning { device } => device.clone(),
                            _ => PathBuf::new(),
                        };
                        this.stage = Stage::Failed { device, message };
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn show_picker(&mut self, cx: &mut Context<Self>) {
        self.delete_confirmation = None;
        self.stage = Stage::Picker;
        cx.notify();
    }

    fn select_node(&mut self, id: NodeId, cx: &mut Context<Self>) {
        let Stage::Ready(ready) = &mut self.stage else {
            return;
        };
        ready.select(id);
        cx.notify();
    }

    fn toggle_expand(&mut self, id: NodeId, cx: &mut Context<Self>) {
        let Stage::Ready(ready) = &mut self.stage else {
            return;
        };
        if !ready.toggle_directory(id) {
            return;
        }
        cx.notify();
    }

    fn reveal_node(&mut self, id: NodeId, cx: &mut Context<Self>) {
        let Stage::Ready(ready) = &mut self.stage else {
            return;
        };
        // Expanding the node itself is deliberately left alone: a treemap
        // click can land in something like /nix/store, and unfolding 200k rows
        // to select one of them is not what the click asked for.
        if let Some(row) = ready.reveal(id) {
            self.tree_scroll
                .scroll_to_item(row, gpui::ScrollStrategy::Center);
        }
        cx.notify();
    }

    fn collapse_all(&mut self, cx: &mut Context<Self>) {
        let Stage::Ready(ready) = &mut self.stage else {
            return;
        };
        ready.collapse_all();
        cx.notify();
    }

    fn delete_selection(&self, id: NodeId) -> Option<DeleteSelection> {
        let Stage::Ready(ready) = &self.stage else {
            return None;
        };
        if id == NodeId::ROOT {
            return None;
        }

        let mount_point = ready.mount_point.clone()?;
        let mount_device = ready.mount_device?;
        let node = ready.scan.result.arena.nodes().get(id.index())?;
        let path = ready.scan.result.arena.path_bytes(id);
        let relative_path = path.strip_prefix(b"/")?;
        let relative_path = PathBuf::from(OsString::from_vec(relative_path.to_vec()));
        let target = DeleteTarget::new(
            mount_point,
            relative_path,
            mount_device,
            node.inode,
            node.kind,
        )
        .ok()?;
        let name = String::from_utf8_lossy(ready.scan.result.arena.name(id)).into_owned();

        Some(DeleteSelection {
            target,
            device: ready.device.clone(),
            name,
            is_directory: node.kind == NodeKind::Directory,
        })
    }

    fn explorer_path(&self, id: NodeId) -> Option<PathBuf> {
        let Stage::Ready(ready) = &self.stage else {
            return None;
        };
        let mount_point = ready.mount_point.clone()?;
        if id == NodeId::ROOT {
            return Some(mount_point);
        }
        let path = ready.scan.result.arena.path_bytes(id);
        let relative_path = path.strip_prefix(b"/")?;
        Some(mount_point.join(PathBuf::from(OsString::from_vec(relative_path.to_vec()))))
    }

    fn request_permanent_delete(&mut self, selection: DeleteSelection, cx: &mut Context<Self>) {
        if self.deletion.is_some() {
            return;
        }
        self.delete_confirmation = Some(selection);
        cx.notify();
    }

    fn cancel_permanent_delete(&mut self, cx: &mut Context<Self>) {
        self.delete_confirmation = None;
        cx.notify();
    }

    fn perform_delete(
        &mut self,
        selection: DeleteSelection,
        mode: DeleteMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.deletion.is_some() {
            return;
        }

        self.delete_confirmation = None;
        self.deletion = Some(DeleteProgress {
            name: selection.name.clone(),
            mode,
        });
        cx.notify();

        let target = selection.target.clone();
        let task = cx.background_executor().spawn(async move {
            file_ops::delete(&target, mode).map_err(|error| error.to_string())
        });

        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.deletion = None;
                match result {
                    Ok(()) => {
                        let message = match mode {
                            DeleteMode::Trash => format!("Moved {} to Trash", selection.name),
                            DeleteMode::Permanent => {
                                format!("Permanently deleted {}", selection.name)
                            }
                        };
                        window.push_notification(Notification::success(message), cx);

                        let still_showing_volume = matches!(
                            &this.stage,
                            Stage::Ready(ready) if ready.device == selection.device
                        );
                        if still_showing_volume {
                            this.start_scan(cx);
                        } else {
                            cx.notify();
                        }
                    }
                    Err(error) => {
                        let message = match mode {
                            DeleteMode::Trash => {
                                format!("Could not move {} to Trash: {error}", selection.name)
                            }
                            DeleteMode::Permanent => {
                                format!("Could not delete {}: {error}", selection.name)
                            }
                        };
                        window.push_notification(Notification::error(message), cx);
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    fn render_delete_confirmation(
        &self,
        selection: &DeleteSelection,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let selection = selection.clone();
        let confirmation_selection = selection.clone();
        let name = selection.name.clone();
        let name_tooltip = name.clone();
        let description = if selection.is_directory {
            "The folder and everything inside it will be deleted. This cannot be undone."
        } else {
            "The file will be deleted. This cannot be undone."
        };

        div()
            .id("delete-confirmation")
            .absolute()
            .inset_0()
            .occlude()
            .flex()
            .items_center()
            .justify_center()
            .p_6()
            .bg(rgb(0x000000).opacity(0.62))
            .child(
                div()
                    .w_full()
                    .max_w(px(440.0))
                    .p_5()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .rounded(cx.theme().radius_lg)
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().table)
                    .shadow_xl()
                    .child(
                        div()
                            .flex()
                            .items_start()
                            .gap_3()
                            .child(
                                div()
                                    .flex_none()
                                    .size_9()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_full()
                                    .bg(cx.theme().danger.opacity(0.14))
                                    .child(
                                        Icon::new(IconName::CircleX)
                                            .small()
                                            .text_color(cx.theme().danger),
                                    ),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_base()
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .child("Delete permanently?"),
                                    )
                                    .child(
                                        div()
                                            .id("delete-confirmation-name")
                                            .w_full()
                                            .overflow_hidden()
                                            .whitespace_nowrap()
                                            .text_ellipsis_middle()
                                            .font_family(cx.theme().mono_font_family.clone())
                                            .text_sm()
                                            .text_color(cx.theme().foreground)
                                            .tooltip(move |window, cx| {
                                                Tooltip::new(name_tooltip.clone()).build(window, cx)
                                            })
                                            .child(name),
                                    )
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(description),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_end()
                            .gap_2()
                            .child(
                                Button::new("cancel-permanent-delete")
                                    .label("Cancel")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.cancel_permanent_delete(cx);
                                    })),
                            )
                            .child(
                                Button::new("confirm-permanent-delete")
                                    .danger()
                                    .label("Delete permanently")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.perform_delete(
                                            confirmation_selection.clone(),
                                            DeleteMode::Permanent,
                                            window,
                                            cx,
                                        );
                                    })),
                            ),
                    ),
            )
    }

    fn render_picker(&self, cx: &mut Context<Self>) -> Div {
        let has_volumes = !self.volumes.is_empty();
        let table = cx.theme().table;
        let selected_bg = cx.theme().primary.opacity(0.1);
        let selected_edge = cx.theme().primary;
        let table_hover = cx.theme().table_hover;
        let row_border = cx.theme().table_row_border.opacity(0.55);
        let foreground = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let mono_font = cx.theme().mono_font_family.clone();
        let rows = self
            .volumes
            .iter()
            .enumerate()
            .map(|(index, volume)| {
                let selected = index == self.selected_volume;
                let hover_bg = if selected {
                    selected_edge.opacity(0.14)
                } else {
                    table_hover
                };
                let mount = volume
                    .mount_point
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "not mounted".to_string());
                div()
                    .id(("volume", index))
                    .h(px(68.0))
                    .flex()
                    .items_center()
                    .border_b_1()
                    .border_color(row_border)
                    .when(selected, |element| element.bg(selected_bg))
                    .cursor_pointer()
                    .hover(move |style| style.bg(hover_bg))
                    .tooltip({
                        let description = format!("{}  ·  {mount}", volume.device.display());
                        move |window, cx| Tooltip::new(description.clone()).build(window, cx)
                    })
                    .on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                        this.selected_volume = index;
                        if event.click_count() >= 2 {
                            this.start_scan(cx);
                        } else {
                            cx.notify();
                        }
                    }))
                    .child(
                        div()
                            .flex_none()
                            .w(px(3.0))
                            .h_full()
                            .when(selected, |element| element.bg(selected_edge)),
                    )
                    .child(
                        div()
                            .ml_3()
                            .flex_none()
                            .size_10()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(cx.theme().radius)
                            .bg(cx.theme().muted.opacity(0.55))
                            .child(
                                Icon::new(IconName::HardDrive)
                                    .text_lg()
                                    .text_color(if selected { selected_edge } else { muted }),
                            ),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .px_3()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(
                                div()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis_middle()
                                    .font_family(mono_font.clone())
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(foreground)
                                    .child(volume.device.display().to_string()),
                            )
                            .child(
                                div()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis_start()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(mount),
                            ),
                    )
                    .child(
                        div()
                            .flex_none()
                            .mr_3()
                            .h_5()
                            .px_2()
                            .flex()
                            .items_center()
                            .rounded_full()
                            .bg(cx.theme().muted.opacity(0.55))
                            .text_xs()
                            .text_color(muted)
                            .child(volume.filesystem.clone()),
                    )
            })
            .collect::<Vec<_>>();

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .items_center()
            .justify_center()
            .p_6()
            .child(
                div()
                    .w_full()
                    .max_w(px(880.0))
                    .max_h(relative(0.82))
                    .min_h(px(360.0))
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .rounded(cx.theme().radius_lg)
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(table)
                    .shadow_sm()
                    .child(
                        div()
                            .flex_none()
                            .min_h(px(88.0))
                            .px_4()
                            .flex()
                            .items_center()
                            .gap_3()
                            .border_b_1()
                            .border_color(cx.theme().border)
                            .child(
                                div()
                                    .flex_none()
                                    .size_10()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(cx.theme().radius)
                                    .bg(cx.theme().muted.opacity(0.55))
                                    .child(
                                        Icon::new(IconName::HardDrive)
                                            .small()
                                            .text_color(cx.theme().foreground),
                                    ),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .flex()
                                    .flex_col()
                                    .gap_0p5()
                                    .child(
                                        div()
                                            .text_base()
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .child("Choose a volume"),
                                    )
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(muted)
                                            .child("Select a mounted volume to analyze."),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scrollbar()
                            .children(rows)
                            .when(!has_volumes, |element| {
                                element.child(
                                    div()
                                        .h_full()
                                        .p_6()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .text_center()
                                        .text_sm()
                                        .text_color(muted)
                                        .child(
                                        "No mounted supported Linux filesystems were found. You can also pass an image or block device as the first argument.",
                                        ),
                                )
                            }),
                    )
                    .child(
                        div()
                            .flex_none()
                            .min_h(px(48.0))
                            .px_3()
                            .flex()
                            .items_center()
                            .gap_2()
                            .border_t_1()
                            .border_color(cx.theme().border)
                            .bg(cx.theme().status_bar)
                            .child(Icon::new(IconName::Info).xsmall().text_color(muted))
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .truncate()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(
                                        "Scanning is read-only. Right-click an item to delete it.",
                                    ),
                            )
                            .when(has_volumes, |element| {
                                element.child(toolbar_button(
                                    "Scan volume",
                                    "scan",
                                    IconName::ChartPie,
                                    true,
                                    cx.listener(|this, _, _, cx| this.start_scan(cx)),
                                ))
                            }),
                    ),
            )
    }

    fn render_scanning(&self, device: &Path, cx: &mut Context<Self>) -> Div {
        let device = device.display().to_string();
        let device_tooltip = device.clone();
        div()
            .flex_1()
            .flex()
            .items_center()
            .justify_center()
            .p_6()
            .child(
                div()
                    .w_full()
                    .max_w(px(460.0))
                    .p_6()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_3()
                    .rounded(cx.theme().radius_lg)
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().table)
                    .shadow_sm()
                    .child(Spinner::new().small().color(cx.theme().primary))
                    .child(
                        div()
                            .id("scanning-path")
                            .max_w_full()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis_start()
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_sm()
                            .text_color(cx.theme().foreground)
                            .tooltip(move |window, cx| {
                                Tooltip::new(device_tooltip.clone()).build(window, cx)
                            })
                            .child(format!("Reading {device}")),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                "Akimi may ask for read access, then scans the volume in parallel.",
                            ),
                    ),
            )
    }

    fn render_error(&self, device: &Path, message: &str, cx: &mut Context<Self>) -> Div {
        div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .p_6()
            .child(
                div()
                    .w_full()
                    .max_w(px(560.0))
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .rounded(cx.theme().radius_lg)
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().table)
                    .shadow_sm()
                    .child(
                        Alert::error("scan-error", message.to_string())
                            .title(format!("Could not read {}", device.display()))
                            .small(),
                    )
                    .child(toolbar_button(
                        "Choose another volume",
                        "error-back",
                        IconName::HardDrive,
                        false,
                        cx.listener(|this, _, _, cx| this.show_picker(cx)),
                    )),
            )
    }

    fn render_results(&self, ready: &ReadyState, cx: &mut Context<Self>) -> Div {
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div().flex_1().min_h_0().p_2().pb_0().child(
                    div()
                        .size_full()
                        .overflow_hidden()
                        .rounded(cx.theme().radius_lg)
                        .border_1()
                        .border_color(cx.theme().border.opacity(0.85))
                        .bg(cx.theme().table)
                        .shadow_sm()
                        .child(
                            v_resizable("results-split")
                                .child(
                                    resizable_panel()
                                        .size(px(290.0))
                                        .size_range(px(140.0)..px(620.0))
                                        .child(self.render_tree(ready.tree.rows().len(), cx)),
                                )
                                .child(
                                    resizable_panel()
                                        .size_range(px(160.0)..Pixels::MAX)
                                        .child(self.render_treemap(ready, cx)),
                                ),
                        ),
                ),
            )
            .child(self.render_status(ready, cx))
    }

    fn render_status(&self, ready: &ReadyState, cx: &mut Context<Self>) -> Div {
        let total = ready.scan.result.totals[ready.selected.index()];
        let selected_kind = ready.scan.result.arena.nodes()[ready.selected.index()].kind;
        let is_dir = selected_kind == NodeKind::Directory;
        // Sparse files and hard links pull the two figures apart; mentioning it
        // only when it is true keeps the bar quiet the rest of the time.
        let logical = (total.recursive_logical != total.recursive_allocated)
            .then(|| format!("{} logical", format_size(total.recursive_logical)));
        let mut details = vec![format!(
            "{} allocated",
            format_size(total.recursive_allocated)
        )];
        if let Some(logical) = logical {
            details.push(logical);
        }
        if is_dir {
            details.push(format!(
                "{} items",
                format_count(total.recursive_items.saturating_sub(1))
            ));
            details.push(format!("{} files", format_count(total.recursive_files)));
            details.push(format!("{} folders", format_count(total.recursive_subdirs)));
        }
        details.push(format!("{} scan", format_duration(ready.elapsed)));
        let details = details.join("  ·  ");
        let details_tooltip = details.clone();

        div().flex_none().child(
            StatusBar::new().child(
                div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(node_icon(
                        selected_kind,
                        ready.tree.is_expanded(ready.selected),
                        cx,
                    ))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .child(self.render_breadcrumb(ready, cx)),
                    )
                    .child(
                        div()
                            .id("selection-summary")
                            .min_w_0()
                            .max_w(relative(0.56))
                            .truncate()
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_color(cx.theme().foreground)
                            .tooltip(move |window, cx| {
                                Tooltip::new(details_tooltip.clone()).build(window, cx)
                            })
                            .child(details),
                    ),
            ),
        )
    }

    fn render_breadcrumb(&self, ready: &ReadyState, cx: &mut Context<Self>) -> Stateful<Div> {
        let chain = ancestor_chain(&ready.scan, ready.selected);
        let last = chain.len().saturating_sub(1);
        let collapse_middle = chain.len() > 5;
        let mut crumbs = Vec::with_capacity(chain.len());
        for (depth, id) in chain.into_iter().enumerate() {
            if collapse_middle && depth == 1 {
                crumbs.push(BreadcrumbItem::new("…").disabled(true));
            }
            if collapse_middle && depth > 0 && depth < last.saturating_sub(2) {
                continue;
            }
            let label = if id == NodeId::ROOT {
                "/".to_string()
            } else {
                String::from_utf8_lossy(ready.scan.result.arena.name(id)).into_owned()
            };
            let is_last = depth == last;
            let target = id;
            let mut crumb = BreadcrumbItem::new(label)
                .max_w(if is_last { px(240.0) } else { px(140.0) })
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis_middle();
            if !is_last {
                crumb =
                    crumb.on_click(cx.listener(move |this, _, _, cx| this.reveal_node(target, cx)));
            }
            crumbs.push(crumb);
        }
        let path = ready.scan.result.arena.display_path(ready.selected);
        div()
            .id("selection-path")
            .min_w_0()
            .flex()
            .items_center()
            .overflow_hidden()
            .tooltip(move |window, cx| Tooltip::new(path.clone()).build(window, cx))
            .child(
                Breadcrumb::new()
                    .min_w_0()
                    .overflow_hidden()
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_xs()
                    .children(crumbs),
            )
    }

    fn render_tree(&self, row_count: usize, cx: &mut Context<Self>) -> Div {
        div()
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(cx.theme().table)
            .child(table_header(cx))
            .child(
                uniform_list(
                    "directory-rows",
                    row_count,
                    cx.processor(|this, range: Range<usize>, _window, cx| {
                        range
                            .filter_map(|index| this.render_tree_row(index, cx))
                            .collect::<Vec<_>>()
                    }),
                )
                .track_scroll(&self.tree_scroll)
                .flex_1(),
            )
    }

    fn render_tree_row(&self, index: usize, cx: &mut Context<Self>) -> Option<AnyElement> {
        let Stage::Ready(ready) = &self.stage else {
            return None;
        };
        let row = *ready.tree.rows().get(index)?;
        let id = row.id;
        let node = ready.scan.result.arena.nodes()[id.index()];
        let total = ready.scan.result.totals[id.index()];
        let is_dir = node.kind == NodeKind::Directory;
        // Percent relative to the direct parent (QDirStat "Subtree Percentage").
        let parent_total = if id == NodeId::ROOT {
            total.recursive_allocated.max(1)
        } else {
            ready.scan.result.totals[node.parent.index()]
                .recursive_allocated
                .max(1)
        };
        let percent = if id == NodeId::ROOT {
            100.0
        } else {
            total.recursive_allocated as f64 * 100.0 / parent_total as f64
        };
        let name = if id == NodeId::ROOT {
            "/".to_string()
        } else {
            String::from_utf8_lossy(ready.scan.result.arena.name(id)).into_owned()
        };
        let is_selected = ready.selected == id;
        let bar_color = share_color(row.depth);
        let indent = px(8.0 + row.depth as f32 * 15.0);
        let foreground = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let row_border = cx.theme().table_row_border.opacity(0.55);
        let row_active = cx.theme().danger.opacity(0.1);
        let row_active_border = cx.theme().danger;
        let row_hover = if is_selected {
            cx.theme().danger.opacity(0.14)
        } else {
            cx.theme().table_hover
        };
        let bar_track = cx.theme().muted;
        let mono_font = cx.theme().mono_font_family.clone();

        let items = if is_dir {
            format_count(total.recursive_items.saturating_sub(1))
        } else {
            String::new()
        };
        let files = if is_dir {
            format_count(total.recursive_files)
        } else {
            String::new()
        };
        let subdirs = if is_dir {
            format_count(total.recursive_subdirs)
        } else {
            String::new()
        };
        let modified = if is_dir {
            format_mtime(total.latest_mtime)
        } else {
            format_mtime(node.mtime)
        };

        let disclosure = {
            if row.expandable {
                Button::new(("twist", id.0 as usize))
                    .ghost()
                    .xsmall()
                    .compact()
                    .icon(if row.expanded {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .tooltip(if row.expanded {
                        "Collapse folder"
                    } else {
                        "Expand folder"
                    })
                    .accessibility_label(if row.expanded {
                        "Collapse folder"
                    } else {
                        "Expand folder"
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_expand(id, cx);
                    }))
                    .into_any_element()
            } else {
                div().flex_none().size(px(20.0)).into_any_element()
            }
        };
        let name_tooltip = name.clone();
        let delete_selection = self.delete_selection(id);
        let explorer_path = self.explorer_path(id);
        let delete_disabled = self.deletion.is_some();
        let owner = cx.weak_entity();

        Some(
            div()
                .id(("row", id.0 as usize))
                .h(px(ROW_H))
                .w_full()
                .flex()
                .items_center()
                .border_b_1()
                .border_color(row_border)
                .font_family(mono_font)
                .text_xs()
                .text_color(foreground)
                .when(is_selected, |element| element.bg(row_active))
                .cursor_pointer()
                .hover(move |style| style.bg(row_hover))
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                        this.select_node(id, cx);
                    }),
                )
                .on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                    if event.click_count() >= 2 {
                        this.toggle_expand(id, cx);
                    } else {
                        this.select_node(id, cx);
                    }
                }))
                .child(
                    div()
                        .flex_none()
                        .w(px(2.0))
                        .h_full()
                        .when(is_selected, |element| element.bg(row_active_border)),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .pl(indent)
                        .pr(px(8.0))
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(disclosure)
                        .child(node_icon(node.kind, row.expanded, cx))
                        .child(
                            div()
                                .id(("node-name", id.0 as usize))
                                .min_w_0()
                                .flex_1()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis_middle()
                                .tooltip(move |window, cx| {
                                    Tooltip::new(name_tooltip.clone()).build(window, cx)
                                })
                                .child(name),
                        ),
                )
                .child(
                    div().w(px(SHARE_W)).flex_none().px_2().child(
                        div()
                            .h(px(4.0))
                            .w_full()
                            .rounded_full()
                            .bg(bar_track)
                            .overflow_hidden()
                            .child(
                                div()
                                    .h_full()
                                    .w(relative((percent.clamp(0.0, 100.0) / 100.0) as f32))
                                    .rounded_full()
                                    .bg(rgb(bar_color)),
                            ),
                    ),
                )
                .child(num_cell(format!("{percent:.1}%"), PERCENT_W, muted, cx))
                .child(num_cell(
                    format_size(total.recursive_allocated),
                    SIZE_W,
                    foreground,
                    cx,
                ))
                .child(num_cell(items, ITEMS_W, muted, cx))
                .child(num_cell(files, FILES_W, muted, cx))
                .child(num_cell(subdirs, FOLDERS_W, muted, cx))
                .child(num_cell(modified, MODIFIED_W, muted, cx))
                .context_menu(move |menu, _, cx| {
                    filesystem_context_menu(
                        menu,
                        explorer_path.clone(),
                        delete_selection.clone(),
                        owner.clone(),
                        delete_disabled,
                        cx.theme().danger,
                    )
                })
                .into_any_element(),
        )
    }

    fn treemap_point(&self, position: Point<Pixels>) -> Option<(f32, f32)> {
        let bounds = self.map_bounds.get();
        let width = f32::from(bounds.size.width);
        let height = f32::from(bounds.size.height);
        if width <= 0.0 || height <= 0.0 {
            return None;
        }
        let x = f32::from(position.x - bounds.origin.x) / width;
        let y = f32::from(position.y - bounds.origin.y) / height;
        ((0.0..=1.0).contains(&x) && (0.0..=1.0).contains(&y)).then_some((x, y))
    }

    fn click_treemap(
        &mut self,
        position: Point<Pixels>,
        click_count: usize,
        cx: &mut Context<Self>,
    ) {
        let Some((x, y)) = self.treemap_point(position) else {
            return;
        };
        if click_count >= 2 {
            let root = {
                let Stage::Ready(ready) = &mut self.stage else {
                    return;
                };
                let scan = ready.scan.clone();
                ready.treemap.zoom_in_at(&scan, x, y)
            };
            let Some(root) = root else {
                return;
            };
            self.reveal_node(root, cx);
            return;
        }
        let id = match &self.stage {
            Stage::Ready(ready) => ready.treemap.hit_test(x, y),
            _ => None,
        };
        let Some(id) = id else {
            return;
        };
        self.reveal_node(id, cx);
    }

    fn scroll_treemap(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        cx.stop_propagation();
        let Some((x, y)) = self.treemap_point(event.position) else {
            return;
        };
        let amount = match event.delta {
            ScrollDelta::Pixels(delta) => ScrollAmount::Pixels(f32::from(delta.y)),
            ScrollDelta::Lines(delta) => ScrollAmount::Lines(delta.y),
        };
        let root = {
            let Stage::Ready(ready) = &mut self.stage else {
                return;
            };
            let scan = ready.scan.clone();
            ready.treemap.scroll(&scan, x, y, amount)
        };
        let Some(root) = root else {
            return;
        };
        self.reveal_node(root, cx);
    }

    fn zoom_treemap_out(&mut self, cx: &mut Context<Self>) {
        let root = {
            let Stage::Ready(ready) = &mut self.stage else {
                return;
            };
            let scan = ready.scan.clone();
            ready.treemap.zoom_out(&scan)
        };
        let Some(root) = root else {
            return;
        };
        self.reveal_node(root, cx);
    }

    fn zoom_treemap_to(&mut self, id: NodeId, cx: &mut Context<Self>) {
        let root = {
            let Stage::Ready(ready) = &mut self.stage else {
                return;
            };
            let scan = ready.scan.clone();
            ready.treemap.zoom_to(&scan, id)
        };
        let Some(root) = root else {
            return;
        };
        self.reveal_node(root, cx);
    }

    fn reset_treemap_zoom(&mut self, cx: &mut Context<Self>) {
        let root = {
            let Stage::Ready(ready) = &mut self.stage else {
                return;
            };
            let scan = ready.scan.clone();
            ready.treemap.reset(&scan)
        };
        let Some(root) = root else {
            return;
        };
        self.reveal_node(root, cx);
    }

    fn render_treemap_zoom_controls(
        &self,
        ready: &ReadyState,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        let root = ready.treemap.root();
        if root == NodeId::ROOT {
            return None;
        }

        let chain = ancestor_chain(&ready.scan, root);
        let last = chain.len().saturating_sub(1);
        let collapse_middle = chain.len() > 5;
        let mut crumbs = Vec::with_capacity(chain.len());
        for (depth, id) in chain.into_iter().enumerate() {
            if collapse_middle && depth == 1 {
                crumbs.push(BreadcrumbItem::new("...").disabled(true));
            }
            if collapse_middle && depth > 0 && depth < last.saturating_sub(2) {
                continue;
            }
            let label = if id == NodeId::ROOT {
                "/".to_string()
            } else {
                String::from_utf8_lossy(ready.scan.result.arena.name(id)).into_owned()
            };
            let target = id;
            let is_last = depth == last;
            let mut crumb = BreadcrumbItem::new(label)
                .max_w(if is_last { px(190.0) } else { px(100.0) })
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis_middle();
            if !is_last {
                crumb = crumb
                    .on_click(cx.listener(move |this, _, _, cx| this.zoom_treemap_to(target, cx)));
            }
            crumbs.push(crumb);
        }

        Some(
            div()
                .id("treemap-zoom-controls")
                .absolute()
                .top_2()
                .left_2()
                .max_w(relative(0.72))
                .h_8()
                .px_1()
                .flex()
                .items_center()
                .gap_1()
                .overflow_hidden()
                .rounded(cx.theme().radius)
                .border_1()
                .border_color(cx.theme().border.opacity(0.9))
                .bg(cx.theme().table.opacity(0.94))
                .shadow_sm()
                .child(
                    Button::new("treemap-zoom-out")
                        .ghost()
                        .xsmall()
                        .compact()
                        .icon(IconName::ChevronLeft)
                        .tooltip("Zoom out")
                        .accessibility_label("Zoom out")
                        .on_click(cx.listener(|this, _, _, cx| this.zoom_treemap_out(cx))),
                )
                .child(
                    div().min_w_0().flex_1().overflow_hidden().child(
                        Breadcrumb::new()
                            .min_w_0()
                            .overflow_hidden()
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_xs()
                            .children(crumbs),
                    ),
                )
                .child(
                    Button::new("treemap-zoom-reset")
                        .ghost()
                        .xsmall()
                        .compact()
                        .icon(IconName::Minimize)
                        .tooltip("Show all files")
                        .accessibility_label("Show all files")
                        .on_click(cx.listener(|this, _, _, cx| this.reset_treemap_zoom(cx))),
                ),
        )
    }

    /// Tile labels are omitted because most would be truncated at this density.
    /// Selection exposes the name through the tree and status bar instead.
    fn render_treemap(&self, ready: &ReadyState, cx: &mut Context<Self>) -> AnyElement {
        let map = ready.treemap.map();
        let empty = map.is_empty();

        // Outline the selected node's own tile, and only that. Items too small
        // to earn a tile were folded into a remainder block; outlining their
        // nearest drawn ancestor instead would misleadingly suggest the
        // selection owns that whole rectangle.
        let outline = map.rect_for(ready.selected);
        let measured = self.map_bounds.clone();
        let selection_color = cx.theme().danger;
        let owner = cx.weak_entity();
        let menu_owner = owner.clone();

        div()
            .id("treemap")
            .size_full()
            .relative()
            .overflow_hidden()
            .bg(cx.theme().table)
            .cursor_pointer()
            .on_click(cx.listener(|this, event: &gpui::ClickEvent, _, cx| {
                this.click_treemap(event.position(), event.click_count(), cx)
            }))
            .on_scroll_wheel(
                cx.listener(|this, event: &ScrollWheelEvent, _, cx| this.scroll_treemap(event, cx)),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    this.click_treemap(event.position, 1, cx)
                }),
            )
            .child(
                canvas(
                    move |bounds, _window, _cx| measured.set(bounds),
                    move |bounds, _, window, _cx| {
                        let origin = bounds.origin;
                        let scale = bounds.size;
                        let place = |rect: Rect| Bounds {
                            origin: point(
                                origin.x + scale.width * rect.x,
                                origin.y + scale.height * rect.y,
                            ),
                            size: size(scale.width * rect.w, scale.height * rect.h),
                        };
                        for tile in map.tiles() {
                            if tile.is_frame() {
                                continue;
                            }
                            let placed = place(tile.rect());
                            let mut fill_bounds = placed;
                            // Trailing-edge bleed: tile edges land on fractional
                            // device pixels, so exactly-adjacent tiles leave
                            // hairline seams of the canvas background showing
                            // through. Overpainting one device pixel on the
                            // right and bottom edges closes those seams; where
                            // tiles truly butt, the neighbour paints over the
                            // bleed.
                            fill_bounds.size.width += px(1.0);
                            fill_bounds.size.height += px(1.0);
                            let (hue, shadow, light) = tile.paint_colors();
                            let min_dim =
                                f32::from(placed.size.width).min(f32::from(placed.size.height));
                            if min_dim >= 3.0 {
                                window.paint_quad(fill(
                                    fill_bounds,
                                    linear_gradient(
                                        135.0,
                                        linear_color_stop(rgb(shadow), 0.0),
                                        linear_color_stop(rgb(light), 1.0),
                                    ),
                                ));
                            } else {
                                window.paint_quad(fill(fill_bounds, rgb(hue)));
                            }
                        }
                        if let Some(rect) = outline {
                            // The stroke starts on the tile's true boundary.
                            // GPUI paints the border inward, so shrinking the
                            // bounds here would leave a visible moat around it.
                            let selected = place(rect);
                            window.paint_quad(quad(
                                selected,
                                px(0.0),
                                transparent_black(),
                                px(2.0),
                                selection_color,
                                BorderStyle::Solid,
                            ));
                        }
                    },
                )
                .absolute()
                .size_full(),
            )
            .children(self.render_treemap_zoom_controls(ready, cx))
            .when(empty, |element| {
                element
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("Nothing on this volume takes up space.")
            })
            .context_menu(move |menu, _, cx| {
                let (selection, disabled) = menu_owner
                    .read_with(cx, |this, _| {
                        let selection = match &this.stage {
                            Stage::Ready(ready) => this.delete_selection(ready.selected),
                            _ => None,
                        };
                        (selection, this.deletion.is_some())
                    })
                    .unwrap_or((None, true));
                let explorer_path = menu_owner
                    .read_with(cx, |this, _| match &this.stage {
                        Stage::Ready(ready) => this.explorer_path(ready.selected),
                        _ => None,
                    })
                    .unwrap_or(None);
                filesystem_context_menu(
                    menu,
                    explorer_path,
                    selection,
                    menu_owner.clone(),
                    disabled,
                    cx.theme().danger,
                )
            })
            .into_any_element()
    }
}

impl Render for Akimi {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let volume = match &self.stage {
            Stage::Picker => None,
            Stage::Scanning { device } | Stage::Failed { device, .. } => {
                Some(device.display().to_string())
            }
            Stage::Ready(ready) => Some(ready.device.display().to_string()),
        };
        let ready = matches!(self.stage, Stage::Ready(_));
        let content = match &self.stage {
            Stage::Picker => self.render_picker(cx).into_any_element(),
            Stage::Scanning { device } => self.render_scanning(device, cx).into_any_element(),
            Stage::Ready(ready) => self.render_results(ready, cx).into_any_element(),
            Stage::Failed { device, message } => {
                self.render_error(device, message, cx).into_any_element()
            }
        };
        let has_volume = volume.is_some();
        let path_tooltip = volume.clone();
        let delete_status = self.deletion.as_ref().map(|progress| match progress.mode {
            DeleteMode::Trash => format!("Moving {} to Trash", progress.name),
            DeleteMode::Permanent => format!("Deleting {}", progress.name),
        });
        let delete_confirmation = self.delete_confirmation.clone();

        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .font_family(cx.theme().font_family.clone())
            .text_color(cx.theme().foreground)
            .child(
                div()
                    .flex_none()
                    .h_12()
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_3()
                    .border_b_1()
                    .border_color(cx.theme().title_bar_border)
                    .bg(cx.theme().title_bar)
                    .child(app_mark(cx))
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_sm()
                            .child("Akimi"),
                    )
                    .children(volume.map(|path| {
                        div()
                            .id("volume-path")
                            .min_w_0()
                            .flex_1()
                            .h_7()
                            .px_2()
                            .flex()
                            .items_center()
                            .gap_2()
                            .overflow_hidden()
                            .rounded(cx.theme().radius)
                            .border_1()
                            .border_color(cx.theme().border.opacity(0.75))
                            .bg(cx.theme().muted.opacity(0.38))
                            .when_some(path_tooltip, |element, tooltip| {
                                element.tooltip(move |window, cx| {
                                    Tooltip::new(tooltip.clone()).build(window, cx)
                                })
                            })
                            .child(
                                Icon::new(IconName::HardDrive)
                                    .xsmall()
                                    .text_color(cx.theme().muted_foreground),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis_start()
                                    .font_family(cx.theme().mono_font_family.clone())
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(path),
                            )
                    }))
                    .when(!has_volume, |element| element.child(div().flex_1()))
                    .when_some(delete_status, |element, status| {
                        let tooltip = status.clone();
                        element.child(
                            div()
                                .id("delete-progress")
                                .max_w(px(220.0))
                                .flex()
                                .items_center()
                                .gap_2()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .tooltip(move |window, cx| {
                                    Tooltip::new(tooltip.clone()).build(window, cx)
                                })
                                .child(Spinner::new().small().color(cx.theme().danger))
                                .child(div().min_w_0().truncate().child(status)),
                        )
                    })
                    .when(ready, |element| {
                        element
                            .child(toolbar_button(
                                "Collapse",
                                "collapse-all",
                                IconName::ChevronsUpDown,
                                false,
                                cx.listener(|this, _, _, cx| this.collapse_all(cx)),
                            ))
                            .child(toolbar_button(
                                "Volumes",
                                "new-scan",
                                IconName::HardDrive,
                                false,
                                cx.listener(|this, _, _, cx| this.show_picker(cx)),
                            ))
                    }),
            )
            .child(content)
            .children(delete_confirmation.map(|selection| {
                self.render_delete_confirmation(&selection, cx)
                    .into_any_element()
            }))
    }
}

fn filesystem_context_menu(
    menu: PopupMenu,
    explorer_path: Option<PathBuf>,
    selection: Option<DeleteSelection>,
    owner: WeakEntity<Akimi>,
    busy: bool,
    danger: Hsla,
) -> PopupMenu {
    let disabled = busy || selection.is_none();
    let open_disabled = busy || explorer_path.is_none();
    let open_owner = owner.clone();
    let trash_selection = selection.clone();
    let trash_owner = owner.clone();
    let permanent_selection = selection;

    menu.item(
        PopupMenuItem::new("Open in File Explorer")
            .icon(IconName::FolderOpen)
            .disabled(open_disabled)
            .on_click(move |_, window, cx| {
                let Some(path) = explorer_path.clone() else {
                    return;
                };
                let open_path = match fs::symlink_metadata(&path) {
                    Ok(metadata) if metadata.is_dir() => path.clone(),
                    _ => path
                        .parent()
                        .map(Path::to_path_buf)
                        .unwrap_or_else(|| path.clone()),
                };
                if let Err(error) = Command::new("xdg-open").arg(&open_path).spawn() {
                    let _ = open_owner.update(cx, |_, cx| {
                        window.push_notification(
                            Notification::error(format!(
                                "Could not open {}: {error}",
                                open_path.display()
                            )),
                            cx,
                        );
                    });
                }
            }),
    )
    .item(PopupMenuItem::separator())
    .item(
        PopupMenuItem::new("Delete")
            .icon(IconName::Delete)
            .disabled(disabled)
            .on_click(move |_, window, cx| {
                let Some(selection) = trash_selection.clone() else {
                    return;
                };
                let _ = trash_owner.update(cx, |this, cx| {
                    this.perform_delete(selection, DeleteMode::Trash, window, cx);
                });
            }),
    )
    .item(PopupMenuItem::separator())
    .item(
        PopupMenuItem::new("Delete permanently...")
            .icon(Icon::new(IconName::CircleX).text_color(danger))
            .disabled(disabled)
            .on_click(move |_, _, cx| {
                let Some(selection) = permanent_selection.clone() else {
                    return;
                };
                let _ = owner.update(cx, |this, cx| {
                    this.request_permanent_delete(selection, cx);
                });
            }),
    )
}

pub(crate) fn run() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(|cx: &mut App| {
            gpui_kit::init(cx);
            Theme::change(ThemeMode::Dark, None, cx);
            let bounds = Bounds::centered(None, size(px(1180.0), px(760.0)), cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    app_id: Some("akimi".to_string()),
                    window_min_size: Some(size(px(760.0), px(560.0))),
                    ..Default::default()
                },
                |window, cx| {
                    window.set_window_title("Akimi");
                    let app = cx.new(|_| Akimi::new());
                    cx.new(|cx| Root::new(app, window, cx))
                },
            )
            .expect("failed to open Akimi window");
            cx.activate(true);
        });
}
