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

/// Toolbar and statistics shown over the remote picture.
pub fn session_overlay(
    ctx: &egui::Context,
    s: &mut Session,
    toolbar_open: bool,
    fullscreen: bool,
    hovering_file: bool,
    actions: &mut Vec<Action>,
) {
    // The bar opens at the top edge and stays open while the pointer is on it
    // (plus a margin), and for a moment after it leaves.
    let state_id = egui::Id::new("toolbar-state");
    let (last_rect, open_until): (Option<egui::Rect>, f64) = ctx.data(|d| d.get_temp(state_id)).unwrap_or((None, 0.0));
    let now = ctx.input(|i| i.time);
    let pointer = ctx.input(|i| i.pointer.hover_pos());
    let near_top = pointer.is_some_and(|p| p.y < 6.0);
    let on_bar = matches!((pointer, last_rect), (Some(p), Some(r)) if r.expand(16.0).contains(p));
    let hold = toolbar_open || near_top || on_bar || ctx.memory(|m| m.any_popup_open());
    let show_bar = hold || now < open_until;
    if show_bar && !hold {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }

    let bar = egui::Area::new(egui::Id::new("toolbar"))
        .anchor(Align2::CENTER_TOP, [0.0, 0.0])
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).inner_margin(egui::Margin::symmetric(10, 4)).show(ui, |ui| {
                if !show_bar {
                    // A thin handle; hovering the top edge (or Ctrl+Alt+Shift+T) opens the bar.
                    ui.label(RichText::new(format!("▾ {}", s.label)).small().weak());
                    return;
                }
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&s.label).strong());
                    ui.separator();

                    if let Some(info) = &s.info {
                        let current = s.stream.as_ref().map(|x| x.display_id).unwrap_or(s.start.display_id);
                        let idx = info.displays.iter().position(|d| d.id == current).map(|i| i + 1).unwrap_or(0);
                        egui::ComboBox::from_id_salt("display").selected_text(format!("显示器 {idx}")).show_ui(ui, |ui| {
                            for (i, d) in info.displays.iter().enumerate() {
                                let text = format!(
                                    "{} · {}x{}{}{}{}",
                                    i + 1,
                                    d.width,
                                    d.height,
                                    if d.primary { " · 主" } else { "" },
                                    if d.is_virtual { " · 虚拟" } else { "" },
                                    if d.hdr { " · HDR" } else { "" }
                                );
                                if ui.selectable_label(d.id == current, text).clicked() {
                                    actions.push(Action::SelectDisplay(d.id));
                                }
                            }
                        });
                    }
                    if s.vd_available() {
                        let current = s.display_choice();
                        let label = match (current.count, current.physical_off, current.block_input) {
                            (0, _, false) => "显示设置".to_string(),
                            (n, true, true) if n > 0 => "隐私屏".to_string(),
                            (0, _, true) => "已屏蔽本地键鼠".to_string(),
                            (n, off, _) => format!("虚拟屏 ×{n}{}", if off { "（物理屏关）" } else { "" }),
                        };
                        ui.menu_button(format!("🖵 {label}"), |ui| {
                            ui.set_min_width(300.0);
                            let mut c = current;
                            if display_choice_ui(ui, &mut c) {
                                actions.push(Action::SetDisplayChoice(c));
                            }
                            ui.separator();
                            ui.horizontal(|ui| {
                                if ui.button("隐私屏").on_hover_text("1 个虚拟显示器 + 物理显示器黑屏 + 屏蔽本地键鼠").clicked() {
                                    let c = crate::session::DisplayChoice { count: current.count.max(1), physical_off: true, block_input: true };
                                    actions.push(Action::SetDisplayChoice(c));
                                    ui.close_menu();
                                }
                                if ui.button("恢复被控端原样").clicked() {
                                    actions.push(Action::SetDisplayChoice(Default::default()));
                                    ui.close_menu();
                                }
                            });
                        });
                    } else if s.info.is_some() {
                        ui.add_enabled(false, egui::Button::new("虚拟显示器")).on_disabled_hover_text(if s.vd_supported {
                            "被控端没有安装虚拟显示器驱动（在被控端管理界面“可选组件”中安装）"
                        } else {
                            "被控端版本太旧，不支持虚拟显示器"
                        });
                    }
                    let mut game = s.game;
                    ui.selectable_value(&mut game, false, "办公");
                    ui.selectable_value(&mut game, true, "游戏");
                    if game != s.game {
                        actions.push(Action::SetGameMode(game));
                    }
                    let current = s.bitrate_policy();
                    let cur_key = POLICIES.iter().find(|p| parse_policy(p.0) as i32 == current).map(|p| p.0).unwrap_or("auto");
                    egui::ComboBox::from_id_salt("policy-bar").selected_text(policy_label(cur_key)).show_ui(ui, |ui| {
                        for (key, name, tip) in POLICIES {
                            if ui.selectable_label(key == cur_key, name).on_hover_text(tip).clicked() {
                                actions.push(Action::SetPolicy(parse_policy(key)));
                            }
                        }
                    });
                    ui.separator();

                    let mut mic = s.mic_on();
                    match s.host_mic_device() {
                        Some(dev) => {
                            if ui
                                .toggle_value(&mut mic, "🎤 麦克风")
                                .on_hover_text(format!("本机麦克风 → 被控端“{dev}”。被控端软件请选择 CABLE Output 作为麦克风"))
                                .changed()
                            {
                                actions.push(Action::SetMic(mic));
                            }
                        }
                        None => {
                            ui.add_enabled(false, egui::Button::new("🎤 麦克风"))
                                .on_disabled_hover_text("被控端没有安装虚拟声卡 VB-Cable（可在被控端管理界面“可选组件”中查看）");
                        }
                    }
                    if let Some(g) = &s.gamepads {
                        let n = g.count();
                        if n > 0 {
                            ui.label(RichText::new(format!("🎮 {n}")).color(Color32::LIGHT_GREEN))
                                .on_hover_text("本机手柄已映射为被控端的 Xbox 手柄（窗口在前台时生效）");
                        }
                    }
                    let mut grab = input::grabbed();
                    if ui.toggle_value(&mut grab, "键盘捕获").on_hover_text("Ctrl+Alt+Shift+Q").changed() {
                        actions.push(Action::SetGrab(grab));
                    }
                    let mut rel = s.relative;
                    if ui.toggle_value(&mut rel, "相对鼠标").on_hover_text("游戏用，Ctrl+Alt+Shift+R").changed() {
                        actions.push(Action::Hotkey(Hotkey::ToggleRelative));
                    }
                    let mut fs = fullscreen;
                    if ui.toggle_value(&mut fs, "全屏").on_hover_text("Ctrl+Alt+Shift+F").changed() {
                        actions.push(Action::Hotkey(Hotkey::ToggleFullscreen));
                    }
                    let mut stats = s.show_stats;
                    if ui.toggle_value(&mut stats, "统计").on_hover_text("Ctrl+Alt+Shift+S").changed() {
                        actions.push(Action::Hotkey(Hotkey::ToggleStats));
                    }
                    if s.usb_available() {
                        let mut open = s.usb_open;
                        if ui.toggle_value(&mut open, "USB 设备").on_hover_text("把本机 USB 设备透传到被控端").changed() {
                            actions.push(Action::ToggleUsb);
                        }
                    }
                    if ui.button("发送文件…").on_hover_text("也可以直接把文件拖进窗口").clicked() {
                        actions.push(Action::PickFiles);
                    }
                    if ui.button("Ctrl+Alt+Del").on_hover_text("Ctrl+Alt+Shift+D").clicked() {
                        actions.push(Action::Hotkey(Hotkey::CtrlAltDel));
                    }
                    ui.separator();
                    if ui.button(RichText::new("断开").color(Color32::from_rgb(255, 140, 130))).clicked() {
                        actions.push(Action::Disconnect);
                    }
                });
            });
        });

    let until = if hold { now + 0.8 } else { open_until };
    ctx.data_mut(|d| d.insert_temp(state_id, (Some(bar.response.rect), until)));

    if !s.status.is_empty() {
        egui::Area::new(egui::Id::new("status"))
            .anchor(Align2::CENTER_BOTTOM, [0.0, -24.0])
            .order(egui::Order::Foreground)
            .interactable(false)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.label(&s.status);
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
        if crate::usb::usbipd_exe().is_none() {
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
