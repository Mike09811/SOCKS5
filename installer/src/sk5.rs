//! sk5-server 的部署:二进制安装(下载/本地复制)、配置生成、systemd 服务、防火墙放行。

use crate::cli::{Args, Ctx};
use crate::exec;
use crate::net;
use crate::sys::SysInfo;
use crate::{log, util};
use std::path::Path;
use std::time::Duration;

pub const BIN_PATH: &str = "/usr/local/bin/sk5-server";
pub const CONF_PATH: &str = "/etc/sk5/sk5.conf";
pub const SERVICE_PATH: &str = "/etc/systemd/system/sk5.service";
pub const SERVICE: &str = "sk5";

/// sk5-server 默认从这个 GitHub 仓库的 Release 下载;
/// 可用 --bin-url / 环境变量 SK5_BIN_URL 覆盖。
pub const DEFAULT_RELEASE_REPO: &str = "Mike09811/SOCKS5";

pub struct Sk5Info {
    pub port: u16,
    pub user: String,
    pub pass: String,
    pub auth: bool,
}

pub fn install(ctx: &Ctx, sys: &SysInfo, args: &Args) -> Result<Sk5Info, String> {
    let auth = !args.no_auth;
    let user = args.user.clone().unwrap_or_else(|| "sk5".to_string());
    if user.is_empty() {
        return Err("用户名不能为空".to_string());
    }
    let pass = args.pass.clone().unwrap_or_else(|| util::random_alnum(16));
    let port = args.port.unwrap_or_else(util::random_port);
    if auth && pass.len() < 8 {
        log::warn("密码少于 8 位,建议使用更长的密码");
    }

    install_binary(ctx, sys, args)?;

    exec::write_file(ctx, CONF_PATH, &conf_text(port, auth, &user, &pass), Some(0o600))
        .map_err(|e| format!("写入配置失败: {e}"))?;
    log::ok(format!("配置已写入 {CONF_PATH}"));

    if sys.has_systemd || ctx.dry_run {
        exec::write_file(ctx, SERVICE_PATH, &unit_text(), None)
            .map_err(|e| format!("写入 systemd 服务失败: {e}"))?;
        exec::run(ctx, "systemctl", &["daemon-reload"])
            .map_err(|e| format!("systemctl daemon-reload 失败: {e}"))?;
        exec::run(ctx, "systemctl", &["enable", "--now", SERVICE])
            .map_err(|e| format!("启用 sk5 服务失败: {e}"))?;
        wait_active(ctx)?;
        log::ok("sk5 服务已启动并设为开机自启");
    } else {
        log::warn("未检测到 systemd,已安装二进制与配置,请自行托管进程:");
        log::plain(format!("  {BIN_PATH} --config {CONF_PATH}"));
    }

    if !args.no_firewall {
        firewall(ctx, port);
    }
    Ok(Sk5Info { port, user, pass, auth })
}

fn install_binary(ctx: &Ctx, sys: &SysInfo, args: &Args) -> Result<(), String> {
    if let Some(src) = &args.sk5_bin {
        if src.starts_with("http://") || src.starts_with("https://") {
            net::try_download(ctx, &[src.clone()], BIN_PATH)?;
        } else if ctx.dry_run {
            log::plain(format!("  [dry-run] 复制 {src} -> {BIN_PATH}"));
        } else {
            if !Path::new(src).exists() {
                return Err(format!("本地文件不存在: {src}"));
            }
            std::fs::copy(src, BIN_PATH).map_err(|e| format!("复制 {src} 失败: {e}"))?;
        }
    } else {
        let url = std::env::var("SK5_BIN_URL")
            .ok()
            .or_else(|| args.bin_url.clone())
            .unwrap_or_else(|| default_bin_url(&sys.arch));
        if let Err(e) = net::try_download(ctx, &[url], BIN_PATH) {
            return Err(format!(
                "{e}\n下载 sk5-server 失败。可选解决方案:\n  1) 手动上传二进制后:sk5-install --sk5-bin /path/to/sk5-server\n  2) 指定下载地址:sk5-install --bin-url https://…/sk5-server-{}\n  3) 把编译好的 sk5-server 发布到自己的 GitHub Release,并设置环境变量 SK5_BIN_URL",
                sys.arch
            ));
        }
    }
    if !ctx.dry_run {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(BIN_PATH, std::fs::Permissions::from_mode(0o755))
                .map_err(|e| format!("设置执行权限失败: {e}"))?;
        }
    }
    let ver = exec::probe(BIN_PATH, &["--version"]);
    if ver.contains("sk5-server") {
        log::ok(format!("sk5-server 就绪:{}", ver));
    } else if !ctx.dry_run {
        log::warn("sk5-server --version 未返回预期输出,请确认二进制与系统架构匹配");
    }
    Ok(())
}

pub fn default_bin_url(arch: &str) -> String {
    format!(
        "https://github.com/{}/releases/latest/download/sk5-server-{arch}",
        DEFAULT_RELEASE_REPO
    )
}

pub fn conf_text(port: u16, auth: bool, user: &str, pass: &str) -> String {
    format!(
        "# 由 sk5-install 生成 — sk5 SOCKS5 服务端配置\n# 修改后执行:systemctl restart sk5\nlisten = 0.0.0.0\nport = {port}\nauth = {}\nuser = {user}\npass = {pass}\n",
        if auth { "userpass" } else { "none" }
    )
}

pub fn unit_text() -> String {
    "\
[Unit]
Description=sk5 SOCKS5 proxy server
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=/usr/local/bin/sk5-server --config /etc/sk5/sk5.conf
Restart=on-failure
RestartSec=3
LimitNOFILE=65535
# 允许绑定 <1024 端口,其余权限收紧
AmbientCapabilities=CAP_NET_BIND_SERVICE
CapabilityBoundingSet=CAP_NET_BIND_SERVICE
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
ReadOnlyPaths=/etc/sk5

[Install]
WantedBy=multi-user.target
"
    .to_string()
}

fn wait_active(ctx: &Ctx) -> Result<(), String> {
    if ctx.dry_run {
        return Ok(());
    }
    for _ in 0..30 {
        if exec::probe("systemctl", &["is-active", SERVICE]) == "active" {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    let logs = exec::probe("journalctl", &["-u", SERVICE, "-n", "20", "--no-pager"]);
    Err(format!("sk5 服务未能启动,最近日志:\n{logs}"))
}

fn firewall(ctx: &Ctx, port: u16) {
    if ctx.dry_run {
        log::plain(format!("  [dry-run] 检查防火墙并放行端口 {port}(如启用)"));
        return;
    }
    if exec::have("ufw") {
        let st = exec::probe("ufw", &["status"]);
        if st.contains("Status: active") {
            exec::run_soft(ctx, "ufw", &["allow", &format!("{port}/tcp")]);
            log::ok(format!("ufw 已放行 {port}/tcp"));
            return;
        }
    }
    if exec::have("firewall-cmd") {
        if exec::probe("firewall-cmd", &["--state"]) == "running" {
            exec::run_soft(ctx, "firewall-cmd", &["--permanent", &format!("--add-port={port}/tcp")]);
            exec::run_soft(ctx, "firewall-cmd", &["--reload"]);
            log::ok(format!("firewalld 已放行 {port}/tcp"));
            return;
        }
    }
    log::info(format!(
        "未检测到启用中的 ufw/firewalld,请确认云厂商安全组已放行 TCP {port}"
    ));
}

pub fn uninstall(ctx: &Ctx) -> Result<(), String> {
    log::info("停止并移除 sk5 服务…");
    if Path::new("/run/systemd/system").exists() || ctx.dry_run {
        exec::run_soft(ctx, "systemctl", &["disable", "--now", SERVICE, "sk5-bbrmod"]);
        exec::run_soft(ctx, "systemctl", &["daemon-reload"]);
    } else {
        log::info("未检测到 systemd,直接删除文件");
    }
    exec::remove_path(ctx, SERVICE_PATH);
    exec::remove_path(ctx, "/etc/systemd/system/sk5-bbrmod.service");
    exec::remove_path(ctx, BIN_PATH);
    exec::remove_path(ctx, CONF_PATH);
    exec::remove_path(ctx, "/etc/sk5");
    exec::remove_path(ctx, "/usr/local/share/sk5");
    log::ok("已卸载 sk5(内核与 sysctl 配置保留;如需恢复默认拥塞控制:");
    log::plain("  sysctl -w net.ipv4.tcp_congestion_control=cubic && sysctl -w net.core.default_qdisc=fq_codel");
    Ok(())
}

/// --status:展示当前 BBR 与 sk5 状态。
pub fn status_print() {
    use crate::sys;
    let s = sys::detect();
    log::heading("sk5 状态");
    log::plain(format!("系统      : {}({},{})", s.pretty, s.arch, s.virt));
    log::plain(format!("内核      : {}", s.kernel));
    log::plain(format!("拥塞控制  : {}", s.current_cc));
    log::plain(format!("队列规则  : {}", sys::current_qdisc()));
    log::plain(format!("可用算法  : {}", s.available_cc));

    let active = exec::probe("systemctl", &["is-active", SERVICE]);
    if active.is_empty() {
        log::plain("sk5 服务   : 未安装 systemd 或未部署");
    } else {
        let enabled = exec::probe("systemctl", &["is-enabled", SERVICE]);
        log::plain(format!("sk5 服务   : {active}(开机自启: {enabled})"));
    }

    if let Ok(text) = std::fs::read_to_string(CONF_PATH) {
        let mut port = String::new();
        let mut user = String::new();
        let mut auth = String::new();
        let mut pass = String::new();
        for line in text.lines() {
            if let Some((k, v)) = line.split_once('=') {
                match k.trim() {
                    "port" => port = v.trim().to_string(),
                    "user" => user = v.trim().to_string(),
                    "pass" => pass = v.trim().to_string(),
                    "auth" => auth = v.trim().to_string(),
                    _ => {}
                }
            }
        }
        log::plain(format!("监听配置  : 0.0.0.0:{port}(认证: {auth},用户: {user})"));
        if let Ok(p) = port.parse::<u16>() {
            if p != 0 {
                let (tg, web) = telegram_links(&detect_ip(), p, &user, &pass, auth != "none");
                log::plain(format!("Telegram : {tg}"));
                log::plain(format!("备用     : {web}"));
            }
        }
    }

    let modinfo = exec::probe(
        "sh",
        &["-c", "lsmod | grep -E 'tcp_(tsunami|nanqinlang)' || true"],
    );
    if !modinfo.is_empty() {
        log::ok(format!("魔改 BBR 模块已加载:{}", modinfo));
    } else {
        log::plain("魔改 BBR 模块: 未加载");
    }
}

/// 猜测对外 IP,仅用于展示连接信息。
pub fn detect_ip() -> String {
    let route = exec::probe("ip", &["-4", "route", "get", "1.1.1.1"]);
    if let Some(idx) = route.find(" src ") {
        let rest = &route[idx + 5..];
        if let Some(end) = rest.find(' ') {
            let ip = &rest[..end];
            if !ip.is_empty() {
                return ip.to_string();
            }
        }
    }
    let hi = exec::probe("hostname", &["-I"]);
    if let Some(first) = hi.split_whitespace().next() {
        return first.to_string();
    }
    "<服务器IP>".to_string()
}

/// Telegram SOCKS5 代理一键添加链接(tg:// 深链 + 网页版)。
pub fn telegram_links(ip: &str, port: u16, user: &str, pass: &str, auth: bool) -> (String, String) {
    let base = format!("server={}&port={port}", util::urlencode(ip));
    let params = if auth {
        format!("{base}&user={}&pass={}", util::urlencode(user), util::urlencode(pass))
    } else {
        base
    };
    (format!("tg://socks?{params}"), format!("https://t.me/socks?{params}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conf_text_content() {
        let c = conf_text(12345, true, "u", "p");
        assert!(c.contains("port = 12345"));
        assert!(c.contains("auth = userpass"));
        assert!(c.contains("user = u"));
        assert!(c.contains("pass = p"));
        let c2 = conf_text(1080, false, "u", "p");
        assert!(c2.contains("auth = none"));
    }

    #[test]
    fn unit_text_key_lines() {
        let u = unit_text();
        assert!(u.contains("ExecStart=/usr/local/bin/sk5-server --config /etc/sk5/sk5.conf"));
        assert!(u.contains("Restart=on-failure"));
        assert!(u.contains("WantedBy=multi-user.target"));
    }

    #[test]
    fn telegram_links_shape() {
        let (tg, web) = telegram_links("1.2.3.4", 1080, "u ser", "p@ss&", true);
        assert_eq!(tg, "tg://socks?server=1.2.3.4&port=1080&user=u%20ser&pass=p%40ss%26");
        assert_eq!(web, "https://t.me/socks?server=1.2.3.4&port=1080&user=u%20ser&pass=p%40ss%26");
        let (tg, web) = telegram_links("10.0.0.1", 9050, "", "", false);
        assert_eq!(tg, "tg://socks?server=10.0.0.1&port=9050");
        assert_eq!(web, "https://t.me/socks?server=10.0.0.1&port=9050");
    }

    #[test]
    fn default_bin_url_shape() {
        let u = default_bin_url("x86_64");
        assert!(u.starts_with("https://github.com/"));
        assert!(u.ends_with("/releases/latest/download/sk5-server-x86_64"));
    }
}
