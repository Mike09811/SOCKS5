//! 彩色终端输出(尊重 NO_COLOR 环境变量)。

use std::sync::OnceLock;

fn color_enabled() -> bool {
    static C: OnceLock<bool> = OnceLock::new();
    *C.get_or_init(|| std::env::var("NO_COLOR").is_err())
}

fn paint(code: &str, sym: &str, msg: &str) {
    if color_enabled() {
        println!("\x1b[{code}m{sym}\x1b[0m {msg}");
    } else {
        println!("{sym} {msg}");
    }
}

pub fn ok(msg: impl std::fmt::Display) {
    paint("32", "✔", &msg.to_string());
}
pub fn warn(msg: impl std::fmt::Display) {
    paint("33", "▲", &msg.to_string());
}
pub fn err(msg: impl std::fmt::Display) {
    paint("31", "✘", &msg.to_string());
}
pub fn info(msg: impl std::fmt::Display) {
    paint("36", "ℹ", &msg.to_string());
}
pub fn plain(msg: impl std::fmt::Display) {
    println!("{}", msg);
}

/// 步骤标题:[1/4] xxx
pub fn step(i: usize, total: usize, msg: impl std::fmt::Display) {
    if color_enabled() {
        println!("\n\x1b[1;36m[{i}/{total}]\x1b[0m {msg}");
    } else {
        println!("\n[{i}/{total}] {msg}");
    }
}

pub fn heading(msg: impl std::fmt::Display) {
    if color_enabled() {
        println!("\n\x1b[1m══ {msg} ══\x1b[0m");
    } else {
        println!("\n== {msg} ==");
    }
}
