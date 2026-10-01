//! egui overlay of a session: toolbar, statistics, transfers, USB window.
//! (The launcher is a web page: see `app/launcher.rs` and common/web.)
//! Screens only describe the UI and return [`Action`]s; the app carries them out.

use egui::{Align2, Color32, RichText};
use nya_ui::egui;

use crate::events::Hotkey;
use crate::input;
use crate::session::{Session, TransferState};

pub enum Action {
    Hotkey(Hotkey),
    SetGameMode(bool),
    SelectDisplay(u32),
    /// Show this host display in an extra window.
    OpenWindow(u32),
    /// Add a virtual screen to the host and show it in a new window.
    NewVirtualWindow,
    SetDisplayChoice(crate::session::DisplayChoice),
    SetGrab(bool),
    SetPolicy(nya_proto::pb::BitratePolicy),
    SetMic(bool),
    ToggleUsb,
    InstallUsbipd,
    RefreshUsb,
    UsbAttach { busid: String, description: String, bound: bool },
    UsbDetach(String),
    Disconnect,
    PickFiles,
    AcceptOffer(u64),
    DismissOffer(u64),
    DismissTransfer(u64),
    OpenFolder(std::path::PathBuf),
}

/// The settings key of a policy (inverse of `parse_policy`).
pub fn policy_key(p: nya_proto::pb::BitratePolicy) -> &'static str {
    use nya_proto::pb::BitratePolicy as P;
    match p {
        P::Quality => "quality",
        P::Balanced => "balanced",
        P::Smooth => "smooth",
        P::Fixed => "fixed",
        P::Unspecified => "auto",
    }
}

pub fn parse_policy(s: &str) -> nya_proto::pb::BitratePolicy {
    use nya_proto::pb::BitratePolicy as P;
    match s {
        "quality" => P::Quality,
        "balanced" => P::Balanced,
        "smooth" => P::Smooth,
        "fixed" => P::Fixed,
        _ => P::Unspecified,
    }
}

const POLICIES: [(&str, &str, &str); 5] = [
    ("auto", "自动", "办公模式用“清晰优先”，游戏模式用“均衡”"),
    ("quality", "清晰优先", "只有持续 2 秒以上严重发送不出去才降，最低保留 60%"),
    ("balanced", "均衡", "持续积压或延迟明显上涨时降到实际能发送的速率，最低 35%"),
    ("smooth", "流畅优先", "积压、延迟上涨、丢包都会触发，最低 15%，适合很差的网络"),
    ("fixed", "固定码率", "从不自动调整"),
];

fn policy_label(s: &str) -> &'static str {
    POLICIES.iter().find(|p| p.0 == s).map(|p| p.1).unwrap_or("自动")
}

/// Controls for the host display setup; returns true if something changed.
fn display_choice_ui(ui: &mut egui::Ui, c: &mut crate::session::DisplayChoice) -> bool {
    let before = *c;
    ui.horizontal(|ui| {
        ui.label("虚拟显示器");
        for (n, label) in [(0, "不用"), (1, "1 个"), (2, "2 个"), (3, "3 个"), (4, "4 个")] {
            ui.selectable_value(&mut c.count, n, label);
        }
    })
    .response
    .on_hover_text("在被控端新建虚拟显示器（第一个设为主显示器）。多个时可在“显示器”里切换查看。需要被控端安装“虚拟显示器”组件，并以服务模式运行");
    ui.add_enabled_ui(c.count > 0, |ui| {
        ui.horizontal(|ui| {
            ui.label("被控端物理显示器");
            ui.selectable_value(&mut c.physical_off, false, "保持显示").on_hover_text("物理显示器和虚拟显示器同时存在（扩展屏）");
            ui.selectable_value(&mut c.physical_off, true, "关闭（黑屏）").on_hover_text("只保留虚拟显示器，被控端屏幕黑屏；断开后自动恢复");
        });
    });
    ui.checkbox(&mut c.block_input, "屏蔽被控端本地键盘鼠标").on_hover_text("远程操作时，被控端旁边的人无法操作（Ctrl+Alt+Del 除外）");
    if c.count == 0 {
        c.physical_off = false;
    }
    *c != before
}

fn size_text(b: u64) -> String {
    match b {
        b if b >= 1 << 30 => format!("{:.2} GB", b as f64 / (1u64 << 30) as f64),
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / (1u64 << 20) as f64),
        b if b >= 1 << 10 => format!("{:.0} KB", b as f64 / 1024.0),
        b => format!("{b} B"),
    }
}

const DIM: Color32 = Color32::from_rgb(0x9a, 0xa0, 0xab);
const OK: Color32 = Color32::from_rgb(0x3c, 0xcf, 0x8e);
const DANGER: Color32 = Color32::from_rgb(0xff, 0x8f, 0x86);

fn bar_separator(ui: &mut egui::Ui) {
    ui.add_space(4.0);
    let (r, _) = ui.allocate_exact_size(egui::vec2(1.0, 20.0), egui::Sense::hover());
    ui.painter().rect_filled(r, 0.0, Color32::from_white_alpha(30));
    ui.add_space(4.0);
}

/// Current display, e.g. "屏幕 2 · 虚拟".
fn display_title(s: &Session) -> String {
    let Some(info) = &s.info else { return "显示器".into() };
    let current = s.stream.as_ref().map(|x| x.display_id).unwrap_or(s.start.display_id);
    match info.displays.iter().position(|d| d.id == current) {
        Some(i) => format!("屏幕 {}{}", i + 1, if info.displays[i].is_virtual { " · 虚拟" } else { "" }),
        None => "显示器".into(),
    }
}

/// Host displays to switch to, then the display setup (virtual screens, privacy).
fn displays_menu(ui: &mut egui::Ui, s: &Session, actions: &mut Vec<Action>) {
    ui.set_min_width(330.0);
    ui.label(RichText::new("被控端的显示器").small().color(DIM));
    if let Some(info) = &s.info {
        let current = s.stream.as_ref().map(|x| x.display_id).unwrap_or(s.start.display_id);
        for (i, d) in info.displays.iter().enumerate() {
            let text = format!(
                "屏幕 {} · {}{}   {}×{}{}",
                i + 1,
                if d.is_virtual { "虚拟" } else { "物理" },
                if d.primary { " · 主" } else { "" },
                d.width,
                d.height,
                if d.hdr { " · HDR→SDR" } else { "" }
            );
            ui.horizontal(|ui| {
                if ui.selectable_label(d.id == current, text).clicked() && d.id != current {
                    actions.push(Action::SelectDisplay(d.id));
                    ui.close_menu();
                }
                if s.multi_supported && d.id != current {
                    let open = s.view_of(d.id).is_some();
                    let label = if open { "窗口中" } else { "新窗口" };
                    if ui.small_button(label).on_hover_text(if open { "已在单独的窗口中显示，点击切换过去" } else { "在一个新窗口里同时显示这个屏幕" }).clicked() {
                        actions.push(Action::OpenWindow(d.id));
                        ui.close_menu();
                    }
                }
            });
        }
    }
    if s.multi_supported && s.vd_available() && s.display_choice().count < 4 {
        if ui
            .button("＋ 新建虚拟屏，在新窗口显示")
            .on_hover_text("在被控端多建一个虚拟显示器，分辨率跟随新窗口的大小。可以把几个窗口并排放在本机的同一块屏幕上")
            .clicked()
        {
            actions.push(Action::NewVirtualWindow);
            ui.close_menu();
        }
    }
    ui.separator();
    ui.label(RichText::new("显示设置").small().color(DIM));
    if s.vd_available() {
        let current = s.display_choice();
        let mut c = current;
        if display_choice_ui(ui, &mut c) {
            actions.push(Action::SetDisplayChoice(c));
        }
        ui.horizontal(|ui| {
            let private = crate::session::DisplayChoice { count: current.count.max(1), physical_off: true, block_input: true };
            if ui
                .add_enabled(current != private, egui::Button::new("一键隐私屏"))
                .on_hover_text("虚拟显示器 + 被控端物理显示器黑屏 + 屏蔽本地键鼠")
                .clicked()
            {
                actions.push(Action::SetDisplayChoice(private));
                ui.close_menu();
            }
            if ui.add_enabled(current != Default::default(), egui::Button::new("恢复被控端原样")).clicked() {
                actions.push(Action::SetDisplayChoice(Default::default()));
                ui.close_menu();
            }
        });
    } else if s.info.is_some() {
        ui.label(
            RichText::new(if s.vd_supported {
                "被控端没有安装虚拟显示器（在被控端管理程序“可选组件”中安装）"
            } else {
                "被控端版本太旧，不支持虚拟显示器"
            })
            .color(DIM),
        );
    }
}

/// What the small toolbar of an extra window asks for.
pub enum ExtraAction {
    None,
    ToggleFullscreen,
    Close,
}

/// Toolbar of an extra window (another host display): name, fullscreen, close.
/// Same behaviour as the main bar: a thin tab until the pointer reaches the top.
pub fn extra_overlay(ctx: &egui::Context, title: &str, status: &str, fullscreen: bool) -> ExtraAction {
    let mut action = ExtraAction::None;
    let pointer = ctx.input(|i| i.pointer.hover_pos());
    let near_top = pointer.is_some_and(|p| p.y < 48.0);
    let show = near_top || ctx.memory(|m| m.any_popup_open());
    egui::Area::new(egui::Id::new("extra-toolbar"))
        .anchor(Align2::CENTER_TOP, [0.0, if show { 8.0 } else { 0.0 }])
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            let frame = egui::Frame::NONE.fill(Color32::from_rgba_unmultiplied(28, 30, 36, 235)).stroke(egui::Stroke::new(1.0_f32, Color32::from_white_alpha(18)));
            if !show {
                frame
                    .corner_radius(egui::CornerRadius { nw: 0, ne: 0, sw: 8, se: 8 })
                    .inner_margin(egui::Margin::symmetric(12, 2))
                    .show(ui, |ui| ui.label(RichText::new(format!("▾ {title}")).small().color(DIM)));
                return;
            }
            frame.corner_radius(12).inner_margin(egui::Margin::same(4)).show(ui, |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    ui.label(RichText::new(title).strong().color(Color32::WHITE));
                    bar_separator(ui);
                    let mut fs = fullscreen;
                    if ui.toggle_value(&mut fs, "全屏").changed() {
                        action = ExtraAction::ToggleFullscreen;
                    }
                    if ui.button("关闭窗口").on_hover_text("只关闭这个窗口，不断开连接").clicked() {
                        action = ExtraAction::Close;
                    }
                });
            });
        });
    if !status.is_empty() {
        egui::Area::new(egui::Id::new("extra-status"))
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .order(egui::Order::Foreground)
            .interactable(false)
            .show(ctx, |ui| {
                egui::Frame::NONE.fill(Color32::from_rgba_unmultiplied(28, 30, 36, 235)).corner_radius(10).inner_margin(egui::Margin::symmetric(14, 8)).show(ui, |ui| {
                    ui.label(RichText::new(status).color(Color32::WHITE));
                });
            });
    }
    if pointer.is_some_and(|p| p.y < 60.0) {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
    action
}

/// Less frequent session controls.
fn more_menu(ui: &mut egui::Ui, s: &Session, actions: &mut Vec<Action>) {
    ui.set_min_width(230.0);
    let mut stats = s.show_stats;
    if ui.toggle_value(&mut stats, "统计信息").on_hover_text("Ctrl+Alt+Shift+S").changed() {
        actions.push(Action::Hotkey(Hotkey::ToggleStats));
    }
    let mut rel = s.relative;
    if ui.toggle_value(&mut rel, "相对鼠标（游戏）").on_hover_text("鼠标锁在窗口内，Ctrl+Alt+Shift+R").changed() {
        actions.push(Action::Hotkey(Hotkey::ToggleRelative));
    }
    let mut mic = s.mic_on();
    match s.host_mic_device() {
        Some(dev) => {
            if ui
                .toggle_value(&mut mic, "麦克风")
                .on_hover_text(format!("本机麦克风 → 被控端“{dev}”。被控端软件请选择 CABLE Output 作为麦克风"))
                .changed()
            {
                actions.push(Action::SetMic(mic));
            }
        }
        None => {
            ui.add_enabled(false, egui::Button::new("麦克风"))
                .on_disabled_hover_text("被控端没有安装虚拟声卡（可在被控端管理程序“可选组件”中安装）");
        }
    }
    if s.usb_available() {
        let mut open = s.usb_open;
        if ui.toggle_value(&mut open, "USB 设备透传…").changed() {
            actions.push(Action::ToggleUsb);
            ui.close_menu();
        }
    }
    if let Some(g) = &s.gamepads {
        let n = g.count();
        if n > 0 {
            ui.label(RichText::new(format!("手柄 ×{n} 已映射为被控端 Xbox 手柄")).color(OK));
        }
    }
    ui.separator();
    if ui.button("发送文件…").on_hover_text("也可以直接把文件拖进窗口").clicked() {
        actions.push(Action::PickFiles);
        ui.close_menu();
    }
    if ui.button("发送 Ctrl+Alt+Del").on_hover_text("Ctrl+Alt+Shift+D，需要被控端以服务模式运行").clicked() {
        actions.push(Action::Hotkey(Hotkey::CtrlAltDel));
        ui.close_menu();
    }
    ui.separator();
    let current = s.bitrate_policy();
    let cur_key = POLICIES.iter().find(|p| parse_policy(p.0) as i32 == current).map(|p| p.0).unwrap_or("auto");
    ui.menu_button(format!("网络变差时：{}", policy_label(cur_key)), |ui| {
        for (key, name, tip) in POLICIES {
            if ui.selectable_label(key == cur_key, name).on_hover_text(tip).clicked() {
                actions.push(Action::SetPolicy(parse_policy(key)));
                ui.close_menu();
            }
        }
    });
}

/// A bar at the top of a session window that shows while the pointer is at
/// the top edge or on it (or `pinned`), stays while one of its menus is open,
/// and hides a moment after the pointer leaves; hidden, a thin tab with
/// `tab` remains. `contents` draws the bar and sets its `bool` when a menu of
/// the bar is open. `id` keeps separate bars apart.
pub fn auto_hide_bar(ctx: &egui::Context, id: &str, pinned: bool, tab: &str, contents: impl FnOnce(&mut egui::Ui, &mut bool)) {
    let state_id = egui::Id::new((id, "state"));
    let (last_rect, open_until, menu_was_open): (Option<egui::Rect>, f64, bool) =
        ctx.data(|d| d.get_temp(state_id)).unwrap_or((None, 0.0, false));
    let now = ctx.input(|i| i.time);
    let pointer = ctx.input(|i| i.pointer.hover_pos());
    let near_top = pointer.is_some_and(|p| p.y < 6.0);
    let on_bar = matches!((pointer, last_rect), (Some(p), Some(r)) if r.expand(16.0).contains(p));
    // egui menus (display menu, "⋯") are not popups in egui's memory: the bar
    // remembers that one of its menus was open, or it would hide as soon as
    // the pointer moves down into the menu.
    let hold = pinned || near_top || on_bar || menu_was_open || ctx.memory(|m| m.any_popup_open());
    let show_bar = hold || now < open_until;
    if show_bar && !hold {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
    let mut menu_open = false;
    let bar = egui::Area::new(egui::Id::new(id))
        .anchor(Align2::CENTER_TOP, [0.0, if show_bar { 8.0 } else { 0.0 }])
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            let frame = egui::Frame::NONE
                .fill(Color32::from_rgba_unmultiplied(28, 30, 36, 235))
                .stroke(egui::Stroke::new(1.0_f32, Color32::from_white_alpha(18)))
                .shadow(ui.visuals().popup_shadow);
            if !show_bar {
                // A thin tab at the top edge; hovering it (or Ctrl+Alt+Shift+T) opens the bar.
                frame
                    .corner_radius(egui::CornerRadius { nw: 0, ne: 0, sw: 8, se: 8 })
                    .inner_margin(egui::Margin::symmetric(12, 2))
                    .show(ui, |ui| ui.label(RichText::new(format!("▾ {tab}")).small().color(DIM)));
                return;
            }
            frame.corner_radius(12).inner_margin(egui::Margin::same(4)).show(ui, |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                ui.spacing_mut().button_padding = egui::vec2(9.0, 5.0);
                {
                    // Flat buttons on the bar; menus keep the normal look.
                    let w = &mut ui.visuals_mut().widgets;
                    w.inactive.weak_bg_fill = Color32::TRANSPARENT;
                    w.inactive.bg_fill = Color32::TRANSPARENT;
                    w.hovered.weak_bg_fill = Color32::from_white_alpha(22);
                    w.hovered.bg_fill = Color32::from_white_alpha(22);
                }
                contents(ui, &mut menu_open);
            });
        });
    if menu_open != menu_was_open {
        ctx.request_repaint();
    }
    let until = if hold || menu_open { now + 0.8 } else { open_until };
    // Keep the last shown rectangle: the tab is much smaller than the bar.
    let rect = if show_bar { Some(bar.response.rect) } else { last_rect };
    ctx.data_mut(|d| d.insert_temp(state_id, (rect, until, menu_open)));
}

/// Toolbar and statistics shown over the remote picture.
pub fn session_overlay(
    ctx: &egui::Context,
    s: &mut Session,
    toolbar_open: bool,
    fullscreen: bool,
    hovering_file: bool,
    actions: &mut Vec<Action>,
) {
    let label = s.label.clone();
    auto_hide_bar(
        ctx,
        "toolbar",
        toolbar_open,
        &label,
        |ui, menu_open| {
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                let (dot, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                ui.painter().circle_filled(dot.center(), 3.5, OK);
                ui.add_space(4.0);
                ui.label(RichText::new(&s.label).strong().color(Color32::WHITE));
                bar_separator(ui);

                // egui 0.31 returns a menu's contents only on the frame it closes, so
                // the menu itself reports that it is open.
                let r = ui.menu_button(format!("{} ▾", display_title(s)), |ui| {
                    *menu_open = true;
                    displays_menu(ui, s, actions)
                });
                r.response.on_hover_text("被控端的显示器、虚拟显示器和隐私屏");

                let mode = if s.game { "游戏" } else { "办公" };
                if ui.button(mode).on_hover_text("办公（清晰）/ 游戏（流畅）切换，Ctrl+Alt+Shift+M").clicked() {
                    actions.push(Action::SetGameMode(!s.game));
                }
                let mut grab = input::grabbed();
                if ui.toggle_value(&mut grab, "键盘").on_hover_text("捕获键盘：Win 键等组合键发给被控端，Ctrl+Alt+Shift+Q").changed() {
                    actions.push(Action::SetGrab(grab));
                }
                let mut fs = fullscreen;
                if ui.toggle_value(&mut fs, "全屏").on_hover_text("Ctrl+Alt+Shift+F").changed() {
                    actions.push(Action::Hotkey(Hotkey::ToggleFullscreen));
                }
                let r = ui.menu_button("⋯", |ui| {
                    *menu_open = true;
                    more_menu(ui, s, actions)
                });
                r.response.on_hover_text("更多");
                bar_separator(ui);
                if ui.button(RichText::new("断开").color(DANGER)).on_hover_text("Ctrl+Alt+Shift+X").clicked() {
                    actions.push(Action::Disconnect);
                }
            });
        },
    );

    if !s.status.is_empty() {
        egui::Area::new(egui::Id::new("status"))
            .anchor(Align2::CENTER_BOTTOM, [0.0, -24.0])
            .order(egui::Order::Foreground)
            .interactable(false)
            .show(ctx, |ui| {
                egui::Frame::NONE
                    .fill(Color32::from_rgba_unmultiplied(28, 30, 36, 235))
                    .corner_radius(10)
                    .inner_margin(egui::Margin::symmetric(14, 8))
                    .show(ui, |ui| {
                        ui.label(RichText::new(&s.status).color(Color32::WHITE));
                    });
            });
    }

    transfers_panel(ctx, s, actions);
    if s.usb_open {
        usb_window(ctx, s, actions);
    }

    if hovering_file {
        egui::Area::new(egui::Id::new("drop-hint"))
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .order(egui::Order::Foreground)
            .interactable(false)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).inner_margin(24.0).show(ui, |ui| {
                    ui.label(RichText::new("松开鼠标，把文件发送到被控端").size(20.0).strong());
                    ui.label(RichText::new("保存在被控端的 下载\\NyaRemoteControl，并放入被控端剪贴板").weak());
                });
            });
    }

    if s.show_stats {
        let mut open = true;
        egui::Window::new("统计")
            .open(&mut open)
            .default_pos([16.0, 48.0])
            .resizable(false)
            .collapsible(true)
            .show(ctx, |ui| {
                for l in s.stats_lines() {
                    ui.label(RichText::new(l).monospace());
                }
            });
        if !open {
            actions.push(Action::Hotkey(Hotkey::ToggleStats));
        }
    }
}

/// Bottom-right panel: host file offers and running / finished transfers.
fn transfers_panel(ctx: &egui::Context, s: &Session, actions: &mut Vec<Action>) {
    if s.offers.is_empty() && s.transfers.is_empty() {
        return;
    }
    egui::Area::new(egui::Id::new("transfers"))
        .anchor(Align2::RIGHT_BOTTOM, [-16.0, -16.0])
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            ui.set_max_width(380.0);
            for o in &s.offers {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    let total: u64 = o.files.iter().map(|f| f.size).sum();
                    let first = o.files.first().map(|f| f.name.as_str()).unwrap_or("");
                    let what = if o.files.len() == 1 { first.to_string() } else { format!("{} 等 {} 个文件", first, o.files.len()) };
                    ui.label(RichText::new("被控端复制了文件").strong());
                    ui.label(format!("{what}（{}）", size_text(total)));
                    ui.horizontal(|ui| {
                        if ui.button(RichText::new("下载到本机").strong()).clicked() {
                            actions.push(Action::AcceptOffer(o.transfer_id));
                        }
                        if ui.button("忽略").clicked() {
                            actions.push(Action::DismissOffer(o.transfer_id));
                        }
                    });
                });
            }
            for t in s.transfers.iter().rev().take(4) {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(if t.upload { "↑ 发送" } else { "↓ 下载" }).strong());
                        ui.label(&t.name);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if !matches!(t.state, TransferState::Running) && ui.small_button("✕").clicked() {
                                actions.push(Action::DismissTransfer(t.id));
                            }
                        });
                    });
                    match &t.state {
                        TransferState::Running => {
                            let frac = if t.total > 0 { t.done as f32 / t.total as f32 } else { 0.0 };
                            ui.add(
                                egui::ProgressBar::new(frac)
                                    .text(format!("{} / {}", size_text(t.done), size_text(t.total)))
                                    .desired_width(340.0),
                            );
                        }
                        TransferState::Done(m) => {
                            ui.label(RichText::new(m).color(Color32::LIGHT_GREEN));
                            if let Some(f) = &t.folder {
                                if ui.button("打开文件夹").clicked() {
                                    actions.push(Action::OpenFolder(f.clone()));
                                }
                            }
                        }
                        TransferState::Failed(m) => {
                            ui.label(RichText::new(m).color(Color32::from_rgb(255, 120, 110)));
                        }
                    }
                });
            }
        });
}

fn usb_window(ctx: &egui::Context, s: &Session, actions: &mut Vec<Action>) {
    let mut open = true;
    egui::Window::new("USB 设备透传").open(&mut open).default_pos([60.0, 80.0]).default_width(460.0).show(ctx, |ui| {
        if s.usbipd_present.is_none() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("正在检查本机的 usbipd-win…");
            });
            return;
        }
        if s.usbipd_present == Some(false) {
            ui.label("需要在本机安装 usbipd-win（可选组件，开源免费）。");
            let running = s.usbipd_install.as_ref().is_some_and(|i| i.0);
            ui.horizontal(|ui| {
                if ui.add_enabled(!running, egui::Button::new("一键安装")).on_hover_text("自动下载固定版本、校验后静默安装，会弹出一次管理员权限确认").clicked() {
                    actions.push(Action::InstallUsbipd);
                }
                if ui.button("官网").clicked() {
                    let _ = std::process::Command::new("explorer").arg(crate::usb::DOWNLOAD_URL).spawn();
                }
            });
            if let Some((running, msg)) = &s.usbipd_install {
                ui.horizontal(|ui| {
                    if *running {
                        ui.spinner();
                    }
                    ui.label(msg);
                });
            }
            return;
        }
        ui.label(RichText::new("透传后设备在本机暂时不可用，断开或结束会话后自动归还。首次共享某个设备会请求一次管理员权限。").weak().small());
        if ui.button("刷新").clicked() {
            actions.push(Action::RefreshUsb);
        }
        ui.separator();
        match &s.usb_devices {
            None => {
                ui.spinner();
            }
            Some(Err(e)) => {
                ui.label(RichText::new(e).color(Color32::from_rgb(255, 120, 110)));
            }
            Some(Ok(list)) if list.is_empty() => {
                ui.label(RichText::new("没有检测到 USB 设备").weak());
            }
            Some(Ok(list)) => {
                egui::Grid::new("usb").num_columns(3).spacing([12.0, 8.0]).striped(true).show(ui, |ui| {
                    for d in list {
                        ui.label(format!("{}  [{}]", d.description, d.busid));
                        let (attached, msg) = s.usb_state.get(&d.busid).cloned().unwrap_or((false, String::new()));
                        if s.usb_busy.contains(&d.busid) {
                            ui.spinner();
                        } else if attached {
                            ui.label(RichText::new("已透传").color(Color32::LIGHT_GREEN));
                        } else if !msg.is_empty() {
                            ui.label(RichText::new(&msg).color(Color32::from_rgb(255, 160, 120)).small());
                        } else if d.in_use {
                            ui.label(RichText::new("正被其他 USB/IP 客户端使用").weak().small());
                        } else {
                            ui.label("");
                        }
                        ui.add_enabled_ui(!s.usb_busy.contains(&d.busid), |ui| {
                            if attached {
                                if ui.button("停止").clicked() {
                                    actions.push(Action::UsbDetach(d.busid.clone()));
                                }
                            } else if ui.button("透传").clicked() {
                                actions.push(Action::UsbAttach {
                                    busid: d.busid.clone(),
                                    description: d.description.clone(),
                                    bound: d.bound,
                                });
                            }
                        });
                        ui.end_row();
                    }
                });
            }
        }
    });
    if !open {
        actions.push(Action::ToggleUsb);
    }
}

#[cfg(test)]
mod bar_tests {
    //! The auto-hiding bar, driven headlessly with simulated pointer input.

    use super::*;
    use std::cell::Cell;

    struct Sim {
        ctx: egui::Context,
        time: f64,
        pos: egui::Pos2,
        /// What the last frame drew.
        bar: Cell<bool>,
        menu: Cell<bool>,
        button: Cell<Option<egui::Rect>>,
        menu_rect: Cell<Option<egui::Rect>>,
    }

    impl Sim {
        fn new() -> Self {
            Self {
                ctx: egui::Context::default(),
                time: 0.0,
                pos: egui::pos2(500.0, 300.0),
                bar: Cell::new(false),
                menu: Cell::new(false),
                button: Cell::new(None),
                menu_rect: Cell::new(None),
            }
        }

        fn frame(&mut self, dt: f64, events: Vec<egui::Event>) {
            self.time += dt;
            let mut all = vec![egui::Event::PointerMoved(self.pos)];
            all.extend(events);
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 700.0))),
                time: Some(self.time),
                events: all,
                ..Default::default()
            };
            self.bar.set(false);
            self.menu.set(false);
            let _ = self.ctx.run(input, |ctx| {
                auto_hide_bar(ctx, "toolbar", false, "host", |ui, menu_open| {
                    self.bar.set(true);
                    ui.horizontal(|ui| {
                        ui.label("host");
                        let r = ui.menu_button("屏幕 1 ▾", |ui| {
                            *menu_open = true;
                            self.menu.set(true);
                            for i in 0..6 {
                                let _ = ui.button(format!("item {i}"));
                            }
                            self.menu_rect.set(Some(ui.min_rect()));
                        });
                        self.button.set(Some(r.response.rect));
                    });
                });
            });
        }

        fn move_to(&mut self, p: egui::Pos2) {
            self.pos = p;
            self.frame(0.016, vec![]);
        }

        fn click(&mut self) {
            let pos = self.pos;
            let press = |pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
            self.frame(0.016, vec![press(true)]);
            self.frame(0.05, vec![press(false)]);
        }

        fn wait(&mut self, secs: f64) {
            for _ in 0..(secs / 0.1) as usize {
                self.frame(0.1, vec![]);
            }
        }
    }

    #[test]
    fn stays_while_its_menu_is_open() {
        let mut s = Sim::new();
        s.frame(0.0, vec![]);
        s.wait(1.5);
        assert!(!s.bar.get(), "hidden at first");
        s.move_to(egui::pos2(500.0, 2.0));
        s.move_to(egui::pos2(500.0, 2.0));
        assert!(s.bar.get(), "shown at the top edge");
        let b = s.button.get().unwrap();
        s.move_to(b.center());
        s.click();
        assert!(s.menu.get(), "menu opened");
        // Down into the menu, well below the bar, and linger there.
        let m = s.menu_rect.get().unwrap();
        s.move_to(egui::pos2(m.center().x, m.bottom() - 5.0));
        s.wait(3.0);
        assert!(s.bar.get() && s.menu.get(), "bar and menu stay while the menu is open");
        // Close the menu by clicking elsewhere; the bar hides after the delay.
        s.move_to(egui::pos2(900.0, 650.0));
        s.click();
        s.wait(2.0);
        assert!(!s.menu.get() && !s.bar.get(), "hidden again after the menu closed");
    }

    #[test]
    fn hides_after_the_pointer_leaves() {
        let mut s = Sim::new();
        s.move_to(egui::pos2(500.0, 2.0));
        s.move_to(egui::pos2(500.0, 20.0));
        assert!(s.bar.get());
        s.move_to(egui::pos2(500.0, 400.0));
        s.wait(2.0);
        assert!(!s.bar.get());
    }
}
