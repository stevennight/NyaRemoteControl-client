//! egui screens: the launcher (host list, settings, dialogs) and the
//! in-session toolbar / statistics window. Screens only describe the UI and
//! return [`Action`]s; the app carries them out.

use egui::{Align2, Color32, RichText};
use nya_ui::egui;

use crate::config::{ClientConfig, Defaults};
use crate::events::Hotkey;
use crate::input;
use crate::session::Session;

pub enum Action {
    Connect { target: String, name: Option<String> },
    CancelConnect,
    PairCode(Option<String>),
    /// Certificate changed: re-verify (true) or give up (false).
    PinChanged(bool),
    /// Fingerprint check after re-verification without pairing.
    FingerprintOk(bool),
    DeleteHost(usize),
    SaveConfig,
    Hotkey(Hotkey),
    SetGameMode(bool),
    SelectDisplay(u32),
    SetGrab(bool),
    Disconnect,
}

pub enum Notice {
    Info(String),
    Error(String),
}

#[derive(Default)]
pub struct LauncherState {
    pub new_address: String,
    pub new_name: String,
    pub notice: Option<Notice>,
    /// Label of the host being connected to.
    pub connecting: Option<String>,
    pub pairing: Option<PairingDialog>,
    pub pin_changed: bool,
    /// Fingerprint to confirm by eye.
    pub verify_fingerprint: Option<String>,
    pub confirm_delete: Option<usize>,
    pub settings_dirty: bool,
}

pub struct PairingDialog {
    pub code: String,
}

fn dialog(title: &str) -> egui::Window<'_> {
    egui::Window::new(title)
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .min_width(380.0)
}

pub fn launcher(ctx: &egui::Context, st: &mut LauncherState, cfg: &mut ClientConfig, actions: &mut Vec<Action>) {
    let busy = st.connecting.is_some() || st.pairing.is_some() || st.pin_changed || st.verify_fingerprint.is_some();
    egui::CentralPanel::default()
        .frame(egui::Frame::central_panel(&ctx.style()).inner_margin(28.0))
        .show(ctx, |ui| {
            ui.add_enabled_ui(!busy, |ui| {
                ui.heading("NyaRemoteControl");
                ui.label(RichText::new("远程桌面客户端").weak());
                ui.add_space(12.0);

                if let Some(n) = &st.notice {
                    let (text, color) = match n {
                        Notice::Info(t) => (t.as_str(), Color32::LIGHT_GREEN),
                        Notice::Error(t) => (t.as_str(), Color32::from_rgb(255, 120, 110)),
                    };
                    egui::Frame::group(ui.style()).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(RichText::new(text).color(color));
                    });
                    ui.add_space(8.0);
                }

                ui.label(RichText::new("被控端").strong());
                egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                    if cfg.hosts.is_empty() {
                        ui.label(RichText::new("还没有保存的被控端，在下方添加。").weak());
                    }
                    for (i, h) in cfg.hosts.iter().enumerate() {
                        egui::Frame::group(ui.style()).show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.vertical(|ui| {
                                    ui.label(RichText::new(&h.name).strong().size(16.0));
                                    ui.label(
                                        RichText::new(format!(
                                            "{}  ·  {}",
                                            h.address,
                                            if h.fingerprint.is_empty() { "未配对" } else { "已配对" }
                                        ))
                                        .weak(),
                                    );
                                });
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.button("删除").clicked() {
                                        st.confirm_delete = Some(i);
                                    }
                                    if ui.button(RichText::new("连接").strong()).clicked() {
                                        actions.push(Action::Connect { target: h.name.clone(), name: None });
                                    }
                                });
                            });
                        });
                    }
                });

                ui.add_space(10.0);
                ui.label(RichText::new("添加被控端").strong());
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut st.new_address)
                            .hint_text("地址，例如 100.64.0.2 或 host:47100")
                            .desired_width(280.0),
                    );
                    ui.add(egui::TextEdit::singleline(&mut st.new_name).hint_text("名称（可选）").desired_width(160.0));
                    let ok = !st.new_address.trim().is_empty();
                    if ui.add_enabled(ok, egui::Button::new("连接")).clicked() {
                        let name = st.new_name.trim();
                        actions.push(Action::Connect {
                            target: st.new_address.trim().to_owned(),
                            name: (!name.is_empty()).then(|| name.to_owned()),
                        });
                    }
                });

                ui.add_space(10.0);
                egui::CollapsingHeader::new(RichText::new("连接设置").strong()).show(ui, |ui| {
                    if settings(ui, &mut cfg.defaults) {
                        st.settings_dirty = true;
                    }
                    if st.settings_dirty && ui.button("保存设置").clicked() {
                        actions.push(Action::SaveConfig);
                        st.settings_dirty = false;
                    }
                });

                ui.add_space(10.0);
                ui.label(
                    RichText::new("连接后：Ctrl+Alt+Shift+T 显示工具条 · S 统计 · Q 释放键盘 · F 全屏 · X 断开").weak().small(),
                );
            });
        });

    if let Some(label) = &st.connecting {
        if st.pairing.is_none() && !st.pin_changed && st.verify_fingerprint.is_none() {
            dialog("连接中").show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(format!("正在连接 {label} …"));
                });
                if ui.button("取消").clicked() {
                    actions.push(Action::CancelConnect);
                }
            });
        }
    }

    if let Some(p) = &mut st.pairing {
        dialog("首次连接：配对").show(ctx, |ui| {
            ui.label("请输入被控端的配对码。");
            ui.label(RichText::new("在被控端运行 nya-server 图形界面，或执行 nya-server pair 查看。").weak());
            let r = ui.add(egui::TextEdit::singleline(&mut p.code).hint_text("XXXX-XXXX-XXXX-XXXX-XXXX-XXXX").desired_width(340.0));
            r.request_focus();
            ui.horizontal(|ui| {
                let submit = ui.button(RichText::new("配对").strong()).clicked()
                    || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                if submit && !p.code.trim().is_empty() {
                    actions.push(Action::PairCode(Some(p.code.trim().to_owned())));
                }
                if ui.button("取消").clicked() {
                    actions.push(Action::PairCode(None));
                }
            });
        });
    }

    if st.pin_changed {
        dialog("被控端证书已变化").show(ctx, |ui| {
            ui.label("被控端的证书和上次保存的不一样。");
            ui.label(RichText::new("常见原因：被控端从开发模式改为服务模式，或重装过。也可能有人在冒充被控端。").weak());
            ui.horizontal(|ui| {
                if ui.button(RichText::new("重新验证").strong()).clicked() {
                    actions.push(Action::PinChanged(true));
                }
                if ui.button("取消").clicked() {
                    actions.push(Action::PinChanged(false));
                }
            });
        });
    }

    if let Some(fp) = &st.verify_fingerprint {
        dialog("核对证书指纹").show(ctx, |ui| {
            ui.label("被控端已经认识本机，所以没有用配对码验证它的身份。请核对指纹：");
            ui.label(RichText::new(fp).monospace().size(20.0).strong());
            ui.label(RichText::new("与被控端界面（或 nya-server pair）显示的“证书指纹”一致才继续。").weak());
            ui.horizontal(|ui| {
                if ui.button(RichText::new("一致，继续").strong()).clicked() {
                    actions.push(Action::FingerprintOk(true));
                }
                if ui.button("不一致，取消").clicked() {
                    actions.push(Action::FingerprintOk(false));
                }
            });
        });
    }

    if let Some(i) = st.confirm_delete {
        let name = cfg.hosts.get(i).map(|h| h.name.clone()).unwrap_or_default();
        dialog("删除被控端").show(ctx, |ui| {
            ui.label(format!("删除“{name}”？之后再连接需要重新配对。"));
            ui.horizontal(|ui| {
                if ui.button("删除").clicked() {
                    actions.push(Action::DeleteHost(i));
                    st.confirm_delete = None;
                }
                if ui.button("取消").clicked() {
                    st.confirm_delete = None;
                }
            });
        });
    }
}

/// Connection settings editor; returns true if something changed.
fn settings(ui: &mut egui::Ui, d: &mut Defaults) -> bool {
    let before = format!("{d:?}");
    egui::Grid::new("settings").num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
        ui.label("模式");
        ui.horizontal(|ui| {
            ui.selectable_value(&mut d.mode, "office".into(), "办公（清晰）");
            ui.selectable_value(&mut d.mode, "game".into(), "游戏（流畅）");
        });
        ui.end_row();

        ui.label("码率");
        ui.horizontal(|ui| {
            let mut auto = d.bitrate_kbps == 0;
            if ui.checkbox(&mut auto, "自动").changed() {
                d.bitrate_kbps = if auto { 0 } else { 10_000 };
            }
            if !auto {
                ui.add(egui::Slider::new(&mut d.bitrate_kbps, 1_000..=80_000).suffix(" kbps").logarithmic(true));
            }
        });
        ui.end_row();

        ui.label("编码");
        egui::ComboBox::from_id_salt("codec").selected_text(codec_label(&d.codec)).show_ui(ui, |ui| {
            for c in ["auto", "hevc", "h264", "av1"] {
                ui.selectable_value(&mut d.codec, c.into(), codec_label(c));
            }
        });
        ui.end_row();

        ui.label("色度");
        egui::ComboBox::from_id_salt("chroma").selected_text(chroma_label(&d.chroma)).show_ui(ui, |ui| {
            for c in ["auto", "444", "420"] {
                ui.selectable_value(&mut d.chroma, c.into(), chroma_label(c));
            }
        });
        ui.end_row();

        ui.label("被控端编码器");
        egui::ComboBox::from_id_salt("encoder").selected_text(encoder_label(&d.encoder)).show_ui(ui, |ui| {
            for c in ["auto", "nvenc", "qsv", "amf", "software"] {
                ui.selectable_value(&mut d.encoder, c.into(), encoder_label(c));
            }
        });
        ui.end_row();

        ui.label("帧率上限");
        ui.horizontal(|ui| {
            let mut auto = d.max_fps == 0;
            if ui.checkbox(&mut auto, "跟随显示器").changed() {
                d.max_fps = if auto { 0 } else { 60 };
            }
            if !auto {
                ui.add(egui::Slider::new(&mut d.max_fps, 15..=240).suffix(" fps"));
            }
        });
        ui.end_row();

        ui.label("其他");
        ui.vertical(|ui| {
            ui.checkbox(&mut d.fullscreen, "连接后全屏");
            ui.checkbox(&mut d.audio, "播放被控端声音");
            ui.checkbox(&mut d.clipboard, "同步剪贴板");
            ui.checkbox(&mut d.hw_decode, "硬件解码（可用时）");
        });
        ui.end_row();
    });
    before != format!("{d:?}")
}

fn codec_label(c: &str) -> &'static str {
    match c {
        "hevc" => "HEVC",
        "h264" => "H.264",
        "av1" => "AV1",
        _ => "自动",
    }
}

fn chroma_label(c: &str) -> &'static str {
    match c {
        "444" => "4:4:4（文字最清晰）",
        "420" => "4:2:0（最省带宽）",
        _ => "自动",
    }
}

fn encoder_label(c: &str) -> &'static str {
    match c {
        "nvenc" => "NVIDIA NVENC",
        "qsv" => "Intel QSV",
        "amf" => "AMD AMF",
        "software" => "软件",
        _ => "自动",
    }
}

/// Toolbar and statistics shown over the remote picture.
pub fn session_overlay(ctx: &egui::Context, s: &mut Session, toolbar_open: bool, fullscreen: bool, actions: &mut Vec<Action>) {
    let near_top = ctx.input(|i| i.pointer.hover_pos()).is_some_and(|p| p.y < 6.0);
    let show_bar = toolbar_open || near_top || ctx.memory(|m| m.any_popup_open());

    egui::Area::new(egui::Id::new("toolbar"))
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
                                let text = format!("{} · {}x{}{}", i + 1, d.width, d.height, if d.primary { " · 主" } else { "" });
                                if ui.selectable_label(d.id == current, text).clicked() {
                                    actions.push(Action::SelectDisplay(d.id));
                                }
                            }
                        });
                    }

                    let mut game = s.game;
                    ui.selectable_value(&mut game, false, "办公");
                    ui.selectable_value(&mut game, true, "游戏");
                    if game != s.game {
                        actions.push(Action::SetGameMode(game));
                    }
                    ui.separator();

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
