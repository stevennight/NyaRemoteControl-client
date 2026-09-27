//! Console launcher: pick a saved host or type an address; pairing prompt.

use std::io::{BufRead, Write};

use crate::config::ClientConfig;

fn read_line(prompt: &str) -> Option<String> {
    print!("{prompt}");
    let _ = std::io::stdout().flush();
    let mut s = String::new();
    std::io::stdin().lock().read_line(&mut s).ok()?;
    let s = s.trim().to_owned();
    (!s.is_empty()).then_some(s)
}

/// Returns the target (saved name or address) to connect to.
pub fn choose(cfg: &ClientConfig) -> Option<String> {
    println!("NyaRemoteControl 客户端");
    if cfg.hosts.is_empty() {
        println!("还没有保存的被控端。");
    } else {
        println!("已保存的被控端：");
        for (i, h) in cfg.hosts.iter().enumerate() {
            println!("  {}. {}  ({})", i + 1, h.name, h.address);
        }
    }
    let input = read_line("输入编号，或输入被控端地址（IP 或 IP:端口）：")?;
    if let Ok(n) = input.parse::<usize>() {
        if n >= 1 && n <= cfg.hosts.len() {
            return Some(cfg.hosts[n - 1].name.clone());
        }
    }
    Some(input)
}

pub fn pairing_code() -> Option<String> {
    println!();
    println!("首次连接需要配对。请在被控端运行 `nya-server pair` 查看配对码");
    println!("（开发模式下配对码显示在 nya-server standalone 的窗口里）。");
    read_line("配对码：")
}
