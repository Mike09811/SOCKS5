//! sk5-install — SOCKS5 代理 + BBR(魔改) 一键安装器。
//!
//! 流程:环境检测 → BBR 加速(魔改 BBR 默认,失败自动回退原生 BBR)
//!      → 部署 sk5-server + systemd 服务 → 输出连接信息。

mod bbr;
mod cli;
mod exec;
mod log;
mod net;
mod sk5;
mod sys;
mod util;

use bbr::Outcome;
use cli::{Args, Ctx};

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() {
    std::process::exit(run());
}

fn run() -> i32 {
    let args = match cli::parse(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(e) if e == "\u{0}version" => {
            println!("sk5-install {VERSION}");
            return 0;
        }
        Err(e) if e.is_empty() => {
            print!("{}", cli::HELP);
            return 0;
        }
        Err(e) => {
            eprintln!("参数错误: {e}\n");
            print!("{}", cli::HELP);
            return 2;
        }
    };

    banner();
    if args.status {
        sk5::status_print();
        return 0;
    }
    if std::env::consts::OS != "linux" && !args.dry_run {
        log::err(format!(
            "sk5-install 仅支持在 Linux 服务器上运行(当前系统 {});开发机预览请加 --dry-run",
            std::env::consts::OS
        ));
        return 1;
    }
    let ctx = Ctx { dry_run: args.dry_run };

    if args.uninstall {
        if !args.yes && !confirm("确认卸载 sk5?(内核与 sysctl 保留)") {
            log::plain("已取消。");
            return 0;
        }
        return match sk5::uninstall(&ctx) {
            Ok(()) => 0,
            Err(e) => {
                log::err(e);
                1
            }
        };
    }

    install_flow(&ctx, &args)
}

fn install_flow(ctx: &Ctx, args: &Args) -> i32 {
    // ---- 1. 环境检测 ----
    log::step(1, 4, "检测系统环境");
    let s = sys::detect();
    log::plain(format!("  系统   : {}({})", s.pretty, s.arch));
    log::plain(format!("  内核   : {}", s.kernel));
    log::plain(format!(
        "  虚拟化 : {}{}",
        if s.virt.is_empty() { "物理机/未知" } else { &s.virt },
        if s.is_container { "(容器环境,无法更换内核)" } else { "" }
    ));
    log::plain(format!("  当前拥塞控制: {}", s.current_cc));

    // ---- 计划确认 ----
    print_plan(ctx, args, &s);
    if !args.yes && !args.dry_run && !confirm("按以上计划安装?") {
        log::plain("已取消。");
        return 0;
    }

    // ---- 2. BBR ----
    log::step(2, 4, format!("配置 BBR 加速({})", args.bbr.label()));
    let report = bbr::install(ctx, &s, args);
    match &report {
        Outcome::Applied(m) => log::ok(m),
        Outcome::NeedReboot(m) => log::ok(m),
        Outcome::Skipped(m) => log::warn(format!("BBR 未启用:{m}")),
    }

    // ---- 3. sk5 ----
    log::step(3, 4, "安装 sk5 SOCKS5 服务端");
    let info = match sk5::install(ctx, &s, args) {
        Ok(i) => i,
        Err(e) => {
            log::err(e);
            return 1;
        }
    };

    // ---- 4. 摘要 ----
    log::step(4, 4, "安装完成");
    summary(ctx, &info, &report);
    0
}

fn print_plan(ctx: &Ctx, args: &Args, s: &sys::SysInfo) {
    log::heading("安装计划");
    let bbr_desc = match args.bbr {
        cli::BbrMode::Off => "跳过".to_string(),
        cli::BbrMode::Native => "原生 BBR(sysctl 直接启用)".to_string(),
        cli::BbrMode::ModBbr => format!(
            "魔改 BBR(4.11.8 内核 + tcp_{} 模块){}",
            match args.bbrmod_variant {
                cli::ModVariant::Tsunami => "tsunami",
                cli::ModVariant::Nanqinlang => "nanqinlang",
            },
            if args.no_fallback { ",失败不回退" } else { ",失败自动回退原生 BBR" }
        ),
        cli::BbrMode::BbrPlus => "BBRPlus(4.14.129-bbrplus 内核)".to_string(),
    };
    let port_desc = match args.port {
        Some(p) => format!("指定端口 {p}"),
        None => "随机端口 20000-60000".to_string(),
    };
    let auth_desc = if args.no_auth { "无认证(公开代理)" } else { "用户名密码认证(默认随机生成)" };
    let bin_desc = match &args.sk5_bin {
        Some(b) => format!("使用指定来源: {b}"),
        None => "从 GitHub Release 下载(可用 --sk5-bin / --bin-url 覆盖)".to_string(),
    };
    log::plain(format!("  BBR        : {bbr_desc}"));
    log::plain(format!("  sk5 服务端  : {bin_desc}"));
    log::plain(format!("  监听       : 0.0.0.0 {port_desc},{auth_desc}"));
    if s.has_systemd || ctx.dry_run {
        log::plain("  系统服务   : systemd(sk5.service,开机自启)");
    } else {
        log::plain("  系统服务   : 未检测到 systemd,仅安装二进制与配置");
    }
}

fn summary(ctx: &Ctx, info: &sk5::Sk5Info, report: &Outcome) {
    let ip = sk5::detect_ip();
    let addr = if info.auth {
        format!("socks5://{}:{}@{}:{}", info.user, info.pass, ip, info.port)
    } else {
        format!("socks5://{ip}:{}", info.port)
    };
    let (tg_link, web_link) = sk5::telegram_links(&ip, info.port, &info.user, &info.pass, info.auth);
    log::heading("sk5 信息");
    log::plain(format!("  SOCKS5 地址  : {addr}"));
    log::plain(format!("  Telegram 指令: {tg_link}"));
    log::plain(format!("  Telegram 备用: {web_link}"));
    log::plain(format!("  配置文件     : {}(修改后 systemctl restart sk5)", sk5::CONF_PATH));
    log::plain("  服务管理     : systemctl status/restart sk5");
    log::plain("  实时日志     : journalctl -u sk5 -f");
    log::plain("  查看状态     : sk5-install --status");
    match report {
        Outcome::Applied(m) => log::plain(format!("  BBR         : {m}")),
        Outcome::NeedReboot(m) => {
            log::warn(format!("  BBR         : {m}"));
            log::warn("  请重启服务器使新内核生效:reboot(sk5 会随系统自动启动)");
        }
        Outcome::Skipped(m) => log::warn(format!("  BBR         : {m}")),
    }
    if ctx.dry_run {
        log::plain("  ※ dry-run 预览模式,以上均未实际执行");
    }
}

fn banner() {
    log::plain(format!(
        "\x1b[1msk5-install v{VERSION}\x1b[0m — SOCKS5 代理 + BBR 加速一键部署"
    ));
}

fn confirm(msg: &str) -> bool {
    use std::io::{BufRead, IsTerminal};
    println!("{msg} [y/N]");
    // curl | bash 场景下 stdin 是管道(读不到用户输入),改从 /dev/tty 读取,
    // 这样管道安装时确认提示仍能显示在终端上等待输入
    let mut line = String::new();
    let n = if std::io::stdin().is_terminal() {
        let stdin = std::io::stdin();
        stdin.lock().read_line(&mut line).unwrap_or(0)
    } else {
        match std::fs::File::open("/dev/tty") {
            Ok(tty) => {
                let mut tty = std::io::BufReader::new(tty);
                tty.read_line(&mut line).unwrap_or(0)
            }
            Err(_) => {
                log::plain("无法交互确认(无可用终端);管道安装请在命令末尾加 -- --yes,例如:");
                log::plain("  curl -fsSL …/install.sh | sudo bash -s -- --yes");
                0
            }
        }
    };
    if n == 0 {
        return false;
    }
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes" | "是")
}
