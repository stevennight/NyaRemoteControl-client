//! Keyboard capture through a low-level hook, so system combinations (Win,
//! Alt+Tab, …) reach the remote while the window is focused and the keyboard
//! is grabbed. Hotkeys are Ctrl+Alt+Shift+<key>.

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};

use nya_proto::pb::{self, input_msg::Ev};
use tokio::sync::mpsc::UnboundedSender;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{MapVirtualKeyW, MAPVK_VK_TO_VSC_EX};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetForegroundWindow, SetWindowsHookExW, HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_EXTENDED,
    LLKHF_INJECTED, LLKHF_UP, WH_KEYBOARD_LL,
};

use crate::events::{Hotkey, NetCmd, Ui, UiEvent};

struct HookState {
    hwnd: AtomicIsize,
    grab: AtomicBool,
    mods: AtomicU8,
    tx: Mutex<Option<UnboundedSender<NetCmd>>>,
    ui: Mutex<Option<Ui>>,
}

static STATE: OnceLock<HookState> = OnceLock::new();

const CTRL: u8 = 1;
const ALT: u8 = 2;
const SHIFT: u8 = 4;

fn state() -> &'static HookState {
    STATE.get_or_init(|| HookState {
        hwnd: AtomicIsize::new(0),
        grab: AtomicBool::new(true),
        mods: AtomicU8::new(0),
        tx: Mutex::new(None),
        ui: Mutex::new(None),
    })
}

pub fn install(hwnd: HWND, tx: UnboundedSender<NetCmd>, ui: Ui) {
    let s = state();
    s.hwnd.store(hwnd.0 as isize, Ordering::SeqCst);
    *s.tx.lock().unwrap() = Some(tx);
    *s.ui.lock().unwrap() = Some(ui);
    unsafe {
        let module = GetModuleHandleW(None).unwrap_or_default();
        if let Err(e) = SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook), module, 0) {
            tracing::error!("keyboard hook: {e}");
        }
    }
}

pub fn set_grab(on: bool) {
    state().grab.store(on, Ordering::SeqCst);
}

pub fn grabbed() -> bool {
    state().grab.load(Ordering::SeqCst)
}

fn hotkey_for(vk: u32) -> Option<Hotkey> {
    Some(match vk {
        0x51 => Hotkey::ToggleGrab,       // Q
        0x53 => Hotkey::ToggleStats,      // S
        0x4D => Hotkey::ToggleMode,       // M
        0x52 => Hotkey::ToggleRelative,   // R
        0x46 => Hotkey::ToggleFullscreen, // F
        0x44 => Hotkey::CtrlAltDel,       // D
        0x58 => Hotkey::Quit,             // X
        0x31..=0x39 => Hotkey::Display((vk - 0x30) as u8),
        _ => return None,
    })
}

fn modifier_bit(vk: u32) -> u8 {
    match vk {
        0x10 | 0xA0 | 0xA1 => SHIFT,
        0x11 | 0xA2 | 0xA3 => CTRL,
        0x12 | 0xA4 | 0xA5 => ALT,
        _ => 0,
    }
}

unsafe extern "system" fn hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let s = state();
        let kb = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
        let ours = GetForegroundWindow().0 as isize == s.hwnd.load(Ordering::Relaxed);
        if ours && kb.flags.0 & LLKHF_INJECTED.0 == 0 {
            let down = kb.flags.0 & LLKHF_UP.0 == 0;
            let bit = modifier_bit(kb.vkCode);
            if bit != 0 {
                if down {
                    s.mods.fetch_or(bit, Ordering::Relaxed);
                } else {
                    s.mods.fetch_and(!bit, Ordering::Relaxed);
                }
            }
            if down && s.mods.load(Ordering::Relaxed) == CTRL | ALT | SHIFT {
                if let Some(h) = hotkey_for(kb.vkCode) {
                    if let Some(ui) = s.ui.lock().unwrap().as_ref() {
                        ui.send(UiEvent::Hotkey(h));
                    }
                    return LRESULT(1);
                }
            }
            if s.grab.load(Ordering::Relaxed) {
                let mut sc = kb.scanCode;
                let mut ext = kb.flags.0 & LLKHF_EXTENDED.0 != 0;
                if sc == 0 {
                    let v = MapVirtualKeyW(kb.vkCode, MAPVK_VK_TO_VSC_EX);
                    sc = v & 0xff;
                    ext = v & 0xff00 == 0xe000;
                }
                if sc != 0 {
                    if let Some(tx) = s.tx.lock().unwrap().as_ref() {
                        let _ = tx.send(NetCmd::Input(pb::InputMsg {
                            ev: Some(Ev::Key(pb::Key { scancode: sc, extended: ext, down })),
                        }));
                    }
                }
                return LRESULT(1);
            }
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}

/// Forget modifier state (focus changes).
pub fn reset_modifiers() {
    state().mods.store(0, Ordering::Relaxed);
}
