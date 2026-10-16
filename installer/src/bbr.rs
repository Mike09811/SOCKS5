//! BBR 安装:原生 BBR / 魔改 BBR(4.11.8 内核 + tsunami/nanqinlang 模块)/ BBRPlus。
//!
//! 魔改 BBR 的原理与经典 tcp.sh 脚本一致:
//!  1. 安装带 BBR 的 4.11.8 内核(来自 cx9208/Linux-NetSpeed 仓库的预编译包);
//!  2. 设置 GRUB 默认启动该内核;
//!  3. 重启后由 sk5-bbrmod.service 在新内核上编译并加载 tcp_tsunami/nanqinlang
//!     拥塞控制模块,并把 sysctl 切换为对应算法(失败则保持原生 BBR)。

use crate::cli::{Args, BbrMode, Ctx, ModVariant};
use crate::exec;
use crate::net;
use crate::sys::{PkgMgr, SysInfo};
use crate::log;

pub const MOD_BBR_KERNEL: &str = "4.11.8";
pub const BBRPLUS_KERNEL: &str = "4.14.129-bbrplus";
pub const SYSCTL_CONF: &str = "/etc/sysctl.d/99-sk5-bbr.conf";
pub const BBRMOD_DIR: &str = "/usr/local/share/sk5/bbrmod";
pub const KERNEL_DIR: &str = "/usr/local/share/sk5/kernel";

const MIRRORS: &[&str] = &[
    "https://raw.githubusercontent.com/cx9208/Linux-NetSpeed/master",
    "https://raw.githubusercontent.com/chiakge/Linux-NetSpeed/master",
    "https://cdn.jsdelivr.net/gh/cx9208/Linux-NetSpeed@master",
];

#[derive(Clone, Debug)]
pub enum Outcome {
    /// 已立即生效(原生 BBR)
    Applied(String),
    /// 内核已装好,需重启后生效(魔改/BBRPlus)
    NeedReboot(String),
    /// 未启用(带原因)
    Skipped(String),
}

pub fn install(ctx: &Ctx, sys: &SysInfo, args: &Args) -> Outcome {
    match args.bbr {
        BbrMode::Off => Outcome::Skipped("已指定 --bbr off,跳过 BBR 安装".into()),
        BbrMode::Native => native(ctx, sys),
        BbrMode::ModBbr | BbrMode::BbrPlus => {
            if distro_too_new_for_modkernel(sys) {
                log::info(format!(
                    "{} 内核 {} 已原生支持 BBR;魔改/BBRPlus 内核(2017-2019 年)在如此新的系统上可能无法引导,自动改用原生 BBR",
                    sys.pretty, sys.kernel
                ));
                return native(ctx, sys);
            }
            let kernel = if args.bbr == BbrMode::ModBbr { MOD_BBR_KERNEL } else { BBRPLUS_KERNEL };
            match modded(ctx, sys, args, kernel) {
                Ok(()) => Outcome::NeedReboot(format!(
                    "已安装 {kernel} 内核,重启后生效(reboot 后 sk5-bbrmod 服务将自动完成模块加载与算法切换)"
                )),
                Err(e) if !args.no_fallback => {
                    log::warn(format!("魔改 BBR 安装失败: {e}"));
                    log::warn("自动回退到原生 BBR …");
                    native(ctx, sys)
                }
                Err(e) => Outcome::Skipped(format!("魔改 BBR 安装失败(--no-fallback 已指定,不回退): {e}")),
            }
        }
    }
}

/// 魔改/BBRPlus 内核包构建于 2017-2019 年(Debian 8/9、Ubuntu 16/18、CentOS 7 时代),
/// 新发行版上可能无法引导或无法编译模块;这些系统自带内核已支持 BBR,直接用原生的。
pub fn distro_too_new_for_modkernel(sys: &SysInfo) -> bool {
    match sys.distro.as_str() {
        "debian" => sys.version.parse::<u32>().map(|v| v >= 11).unwrap_or(false),
        "ubuntu" => sys
            .version
            .split('.')
            .next()
            .and_then(|v| v.parse::<u32>().ok())
            .map(|v| v >= 20)
            .unwrap_or(false),
        _ => false,
    }
}

/// 原生 BBR:modprobe + sysctl,立即生效。
fn native(ctx: &Ctx, sys: &SysInfo) -> Outcome {
    let mut avail = sys.available_cc.clone();
    if !avail.split_whitespace().any(|s| s == "bbr") {
        exec::run_soft(ctx, "modprobe", &["tcp_bbr"]);
        avail = std::fs::read_to_string("/proc/sys/net/ipv4/tcp_available_congestion_control")
            .unwrap_or_default()
            .trim()
            .to_string();
    }
    if !avail.split_whitespace().any(|s| s == "bbr") {
        if kernel_ge(&sys.kernel, 4, 9) {
            return Outcome::Skipped(
                "当前内核 >= 4.9 但未找到可用的 tcp_bbr 模块(可尝试安装发行版更新的内核,或用 --bbr modbbr 换内核)".into(),
            );
        }
        return Outcome::Skipped(format!(
            "当前内核 {} 低于 4.9,不支持原生 BBR(可尝试 --bbr modbbr 更换内核)",
            sys.kernel
        ));
    }
    apply_sysctl(ctx, "bbr")
}

/// 写入 sysctl 配置并立即应用;返回最终状态。
pub fn apply_sysctl(ctx: &Ctx, cc: &str) -> Outcome {
    sanitize_sysctl_conf(ctx);
    if let Err(e) = exec::write_file(ctx, SYSCTL_CONF, &sysctl_conf(cc), None) {
        return Outcome::Skipped(format!("写入 sysctl 配置失败: {e}"));
    }
    exec::run_soft(ctx, "sysctl", &["-w", "net.core.default_qdisc=fq"]);
    exec::run_soft(ctx, "sysctl", &["-w", &format!("net.ipv4.tcp_congestion_control={cc}")]);
    if ctx.dry_run {
        return Outcome::Applied("原生 BBR 配置完成(dry-run 预览)".into());
    }
    let cur = exec::probe("sysctl", &["-n", "net.ipv4.tcp_congestion_control"]);
    if cur.trim() == cc {
        Outcome::Applied(format!("原生 BBR 已启用(拥塞控制 = {cc},队列规则 = fq)"))
    } else {
        Outcome::Skipped(format!(
            "sysctl 未能立即生效(当前 {cur}),配置已写入 {SYSCTL_CONF},重启后生效"
        ))
    }
}

pub fn sysctl_conf(cc: &str) -> String {
    format!(
        "# 由 sk5-install 生成 — TCP 拥塞控制与队列规则\nnet.core.default_qdisc = fq\nnet.ipv4.tcp_congestion_control = {cc}\n"
    )
}

/// /etc/sysctl.conf 中旧的拥塞控制行会在 sysctl --system 时覆盖我们的配置,提前注释掉。
fn sanitize_sysctl_conf(ctx: &Ctx) {
    let p = "/etc/sysctl.conf";
    let Ok(text) = std::fs::read_to_string(p) else { return };
    let keys = ["net.ipv4.tcp_congestion_control", "net.core.default_qdisc"];
    let mut changed = false;
    let new: String = text
        .lines()
        .map(|l| {
            let t = l.trim_start();
            if !t.starts_with('#') && keys.iter().any(|k| t.starts_with(k)) {
                changed = true;
                format!("# [sk5] 已注释 {t}")
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    if changed {
        if let Err(e) = exec::write_file(ctx, p, &(new + "\n"), None) {
            log::warn(format!("注释 /etc/sysctl.conf 旧配置失败: {e}"));
        } else {
            log::info("已注释 /etc/sysctl.conf 中旧的拥塞控制配置,避免覆盖 sk5 的设置");
        }
    }
}

/// 比较 kernel 版本(如 "5.10.0-8-amd64"、"4.14.129-bbrplus")是否 >= major.minor。
pub fn kernel_ge(k: &str, major: u32, minor: u32) -> bool {
    let nums: Vec<u32> = k
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse().ok())
        .take(2)
        .collect();
    match nums.as_slice() {
        [a, b] => (*a, *b) >= (major, minor),
        _ => false,
    }
}

/// 魔改 BBR / BBRPlus 主流程:装内核 → 设 GRUB → 部署开机模块服务。
fn modded(ctx: &Ctx, sys: &SysInfo, args: &Args, kernel: &str) -> Result<(), String> {
    if sys.is_container {
        return Err("容器/OpenVZ 环境共享宿主机内核,无法更换内核(可改用 --bbr native)".into());
    }
    if sys.arch != "x86_64" {
        return Err(format!("魔改 BBR 内核仅提供 x86_64 预编译包(当前架构 {})", sys.arch));
    }
    let files = kernel_urls(sys, kernel);
    if files.is_empty() {
        return Err(
            "当前发行版/版本没有对应的预编译内核包(支持 Debian 8-10 / Ubuntu 14-18 / CentOS 7,均为 x86_64;其他系统请用 --bbr native)".into(),
        );
    }

    log::info("安装编译依赖(gcc / make / perl)…");
    install_build_deps(ctx, sys)?;

    if !ctx.dry_run {
        std::fs::create_dir_all(KERNEL_DIR).map_err(|e| format!("创建 {KERNEL_DIR} 失败: {e}"))?;
    }
    let mut paths: Vec<String> = Vec::new();
    for urls in &files {
        let name = urls[0].rsplit('/').next().unwrap_or("kernel.pkg").to_string();
        let dest = format!("{KERNEL_DIR}/{name}");
        net::try_download(ctx, urls, &dest)?;
        paths.push(dest);
    }

    log::info("安装内核包…");
    let refs: Vec<&str> = paths.iter().map(|s| s.as_str()).collect();
    match sys.pkg {
        PkgMgr::Apt => {
            let mut dpkg_args = vec!["-i"];
            dpkg_args.extend(refs.iter().copied());
            if let Err(e) = exec::run(ctx, "dpkg", &dpkg_args) {
                log::warn(format!("dpkg 报告依赖问题({e}),尝试自动修复…"));
                exec::run(ctx, "apt-get", &["-f", "-y", "install"])
                    .map_err(|e| format!("apt-get -f install 失败: {e}"))?;
                exec::run(ctx, "dpkg", &dpkg_args).map_err(|e| format!("内核包安装失败: {e}"))?;
            }
        }
        PkgMgr::Yum | PkgMgr::Dnf => {
            let pm = if sys.pkg == PkgMgr::Dnf { "dnf" } else { "yum" };
            let mut y_args = vec!["localinstall", "-y"];
            y_args.extend(refs.iter().copied());
            exec::run(ctx, pm, &y_args).map_err(|e| format!("内核包安装失败: {e}"))?;
        }
        _ => return Err("无法确定包管理器,请手动安装内核包后重试".into()),
    }

    let moddir = format!("/lib/modules/{kernel}");
    if !ctx.dry_run && !std::path::Path::new(&moddir).exists() {
        return Err(format!("内核 {kernel} 安装后未发现 {moddir},安装可能不完整"));
    }
    log::ok(format!("内核包 {kernel} 安装完成"));

    setup_grub(ctx, sys, kernel)?;

    // 重启前的安全基线:原生 BBR(若模块编译失败,重启后仍是原生 BBR)
    log::info("写入 BBR 基线 sysctl 配置(原生 bbr,模块加载后再切换为魔改算法)…");
    let _ = apply_sysctl(ctx, "bbr");

    write_bbrmod_assets(ctx, args, kernel)?;
    Ok(())
}

/// 发行版太新时给出提醒(内核/编译器年代差)。(已由 distro_too_new_for_modkernel 取代)

fn install_build_deps(ctx: &Ctx, sys: &SysInfo) -> Result<(), String> {
    match sys.pkg {
        PkgMgr::Apt => {
            exec::run_soft(ctx, "apt-get", &["-y", "update"]);
            exec::run(ctx, "apt-get", &["install", "-y", "gcc", "make", "perl"])
                .map_err(|e| format!("安装编译依赖失败: {e}"))?;
        }
        PkgMgr::Yum | PkgMgr::Dnf => {
            let pm = if sys.pkg == PkgMgr::Dnf { "dnf" } else { "yum" };
            exec::run(ctx, pm, &["install", "-y", "gcc", "make", "perl"])
                .map_err(|e| format!("安装编译依赖失败: {e}"))?;
        }
        _ => return Err("无法确定包管理器,请手动安装 gcc/make 后重试".into()),
    }
    Ok(())
}

/// 生成每个内核包的多镜像候选 URL(外层 = 文件,内层 = 镜像)。
fn kernel_urls(sys: &SysInfo, kernel: &str) -> Vec<Vec<String>> {
    if sys.arch != "x86_64" {
        return vec![]; // 预编译包只有 x64 版本
    }
    let files: Vec<String> = match (sys.distro.as_str(), sys.version.as_str()) {
        ("debian" | "ubuntu", _) if kernel == MOD_BBR_KERNEL => vec![
            "bbr/debian-ubuntu/linux-headers-4.11.8-all.deb".into(),
            "bbr/debian-ubuntu/x64/linux-headers-4.11.8.deb".into(),
            "bbr/debian-ubuntu/x64/linux-image-4.11.8.deb".into(),
        ],
        ("debian" | "ubuntu", _) if kernel == BBRPLUS_KERNEL => vec![
            "bbrplus/debian-ubuntu/x64/linux-headers-4.14.129-bbrplus.deb".into(),
            "bbrplus/debian-ubuntu/x64/linux-image-4.14.129-bbrplus.deb".into(),
        ],
        ("centos" | "rhel", "7") if kernel == MOD_BBR_KERNEL => vec![
            "bbr/centos/7/x64/kernel-ml-4.11.8.rpm".into(),
            "bbr/centos/7/x64/kernel-ml-headers-4.11.8.rpm".into(),
            "bbr/centos/7/x64/kernel-ml-devel-4.11.8.rpm".into(),
        ],
        ("centos" | "rhel", "7") if kernel == BBRPLUS_KERNEL => {
            vec!["bbrplus/centos/7/kernel-4.14.129-bbrplus.rpm".into()]
        }
        _ => vec![],
    };
    if files.is_empty() {
        return vec![];
    }
    files
        .iter()
        .map(|f| MIRRORS.iter().map(|m| format!("{m}/{f}")).collect())
        .collect()
}

fn variant_name(v: ModVariant) -> &'static str {
    match v {
        ModVariant::Tsunami => "tsunami",
        ModVariant::Nanqinlang => "nanqinlang",
    }
}

fn variant_source_urls(variant: &str) -> Vec<String> {
    let cx = MIRRORS[0];
    let chi = MIRRORS[1];
    let js = MIRRORS[2];
    match variant {
        "nanqinlang" => vec![
            format!("{chi}/bbr/tcp_nanqinlang.c"),
            format!("{cx}/bbr/tcp_nanqinlang.c"),
            format!("{cx}/bbr/centos/tcp_nanqinlang.c"),
        ],
        _ => vec![
            format!("{cx}/bbr/tcp_tsunami.c"),
            format!("{chi}/bbr/tcp_tsunami.c"),
            format!("{js}/bbr/tcp_tsunami.c"),
        ],
    }
}

pub fn makefile(variant: &str) -> String {
    format!(
        "obj-m += tcp_{variant}.o\n\nall:\n\tmake -C /lib/modules/$(shell uname -r)/build M=$(PWD) modules\n\nclean:\n\tmake -C /lib/modules/$(shell uname -r)/build M=$(PWD) clean\n"
    )
}

pub fn setup_sh(variant: &str) -> String {
    r#"#!/usr/bin/env bash
# 由 sk5-install 生成 — 开机时针对魔改 BBR 内核编译/加载模块并切换拥塞控制算法
# 当前内核不是目标内核时静默跳过,保持原生 BBR(安全回退)。
set -u
VARIANT="__VARIANT__"
DIR="/usr/local/share/sk5/bbrmod"
KVER="$(uname -r)"
log() { echo "[sk5-bbrmod] $*"; }

apply() { # $1 = 拥塞控制算法
    sysctl -w net.core.default_qdisc=fq
    sysctl -w "net.ipv4.tcp_congestion_control=$1"
    sed -i "s/^net.ipv4.tcp_congestion_control.*/net.ipv4.tcp_congestion_control = $1/" /etc/sysctl.d/99-sk5-bbr.conf
}

case "$KVER" in
    4.14.129-bbrplus*)
        apply bbrplus
        log "BBRPlus 已启用"
        exit 0
        ;;
    4.11.8*)
        ;;
    *)
        log "当前内核 $KVER 不是魔改 BBR 内核,保持原生 BBR"
        exit 0
        ;;
esac

cd "$DIR" || exit 1
if lsmod | grep -q "tcp_${VARIANT}"; then
    apply "$VARIANT"
    log "魔改 BBR(${VARIANT})已启用"
    exit 0
fi
make clean >/dev/null 2>&1
if ! make >/dev/null 2>&1 && ! make KCFLAGS="-fno-pie" >/dev/null 2>&1; then
    log "模块编译失败,保持原生 BBR(需要 gcc/make 与匹配的内核头文件)"
    exit 0
fi
if ! insmod "tcp_${VARIANT}.ko"; then
    log "模块加载失败,保持原生 BBR"
    exit 0
fi
apply "$VARIANT"
log "魔改 BBR(${VARIANT})已启用"
"#
    .replace("__VARIANT__", variant)
}

pub fn bbrmod_unit() -> String {
    "\
[Unit]
Description=sk5 modded-BBR boot setup (build/load congestion control module)
After=systemd-modules-load.target
Before=network-online.target
ConditionPathExists=/usr/local/share/sk5/bbrmod/setup.sh

[Service]
Type=oneshot
ExecStart=/usr/local/share/sk5/bbrmod/setup.sh
RemainAfterExit=yes
TimeoutStartSec=600

[Install]
WantedBy=multi-user.target
"
    .to_string()
}

fn write_bbrmod_assets(ctx: &Ctx, args: &Args, kernel: &str) -> Result<(), String> {
    let variant = variant_name(args.bbrmod_variant);
    if !ctx.dry_run {
        std::fs::create_dir_all(BBRMOD_DIR).map_err(|e| format!("创建 {BBRMOD_DIR} 失败: {e}"))?;
    }
    if kernel == MOD_BBR_KERNEL {
        // BBRPlus 内核自带 bbrplus 算法,不需要编译模块;魔改 BBR 需要模块源码
        let dest = format!("{BBRMOD_DIR}/tcp_{variant}.c");
        net::try_download(ctx, &variant_source_urls(variant), &dest)?;
    }
    exec::write_file(ctx, &format!("{BBRMOD_DIR}/Makefile"), &makefile(variant), None)
        .map_err(|e| format!("写入 Makefile 失败: {e}"))?;
    exec::write_file(ctx, &format!("{BBRMOD_DIR}/setup.sh"), &setup_sh(variant), Some(0o755))
        .map_err(|e| format!("写入 setup.sh 失败: {e}"))?;
    exec::write_file(ctx, "/etc/systemd/system/sk5-bbrmod.service", &bbrmod_unit(), None)
        .map_err(|e| format!("写入 sk5-bbrmod 服务失败: {e}"))?;
    log::info("已部署 sk5-bbrmod 开机服务(重启后自动编译并加载魔改算法模块)");
    Ok(())
}

/// 刷新 GRUB 配置并把默认启动项指向新内核。
fn setup_grub(ctx: &Ctx, sys: &SysInfo, kernel: &str) -> Result<(), String> {
    let is_rhel = matches!(sys.distro.as_str(), "centos" | "rhel");
    let cfg_path = if is_rhel { "/boot/grub2/grub.cfg" } else { "/boot/grub/grub.cfg" };

    log::info("刷新 GRUB 引导配置…");
    if exec::have("update-grub") {
        exec::run(ctx, "update-grub", &[]).map_err(|e| format!("update-grub 失败: {e}"))?;
    } else if exec::have("grub2-mkconfig") {
        exec::run(ctx, "grub2-mkconfig", &["-o", cfg_path])
            .map_err(|e| format!("grub2-mkconfig 失败: {e}"))?;
    } else if exec::have("grub-mkconfig") {
        exec::run(ctx, "grub-mkconfig", &["-o", cfg_path])
            .map_err(|e| format!("grub-mkconfig 失败: {e}"))?;
    } else {
        log::warn("未找到 update-grub / grub-mkconfig,请确认引导为 GRUB2");
    }

    let text = std::fs::read_to_string(cfg_path).unwrap_or_default();
    match find_grub_entry(&text, kernel) {
        Some(entry) => {
            log::info(format!("GRUB 默认启动项 → {entry}"));
            if is_rhel {
                exec::run(ctx, "grub2-set-default", &[&entry])
                    .map_err(|e| format!("grub2-set-default 失败: {e}"))?;
            } else {
                set_grub_default_file(ctx, &entry)?;
                if exec::have("update-grub") {
                    exec::run_soft(ctx, "update-grub", &[]);
                }
            }
        }
        None => {
            log::warn(format!(
                "未在 {cfg_path} 中找到包含 {kernel} 的启动项,请重启时在 GRUB 菜单手动选择该内核"
            ));
        }
    }
    Ok(())
}

/// 在 /etc/default/grub 中设置 GRUB_DEFAULT="submenu>entry"。
fn set_grub_default_file(ctx: &Ctx, entry: &str) -> Result<(), String> {
    let path = "/etc/default/grub";
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let new = replace_grub_default(&text, entry);
    exec::write_file(ctx, path, &new, None).map_err(|e| format!("写入 {path} 失败: {e}"))
}

/// 纯函数:替换或追加 GRUB_DEFAULT 行(便于测试)。
pub fn replace_grub_default(text: &str, entry: &str) -> String {
    let line = format!("GRUB_DEFAULT=\"{entry}\"");
    let mut found = false;
    let mut out: Vec<String> = text
        .lines()
        .map(|l| {
            let t = l.trim_start();
            if t.starts_with("GRUB_DEFAULT=") && !t.starts_with('#') {
                found = true;
                line.clone()
            } else {
                l.to_string()
            }
        })
        .collect();
    if !found {
        out.push(line);
    }
    let mut s = out.join("\n");
    if !s.ends_with('\n') {
        s.push('\n');
    }
    s
}

/// 从 grub.cfg 文本中找到包含指定内核版本的启动项,
/// 返回可直接用于 GRUB_DEFAULT 的 "子菜单>条目" 形式。
pub fn find_grub_entry(cfg: &str, needle: &str) -> Option<String> {
    let mut submenu: Option<String> = None;
    for line in cfg.lines() {
        let t = line.trim_start();
        if t.starts_with("submenu ") {
            submenu = extract_quoted(t);
            continue;
        }
        if t.starts_with("menuentry ") {
            if let Some(title) = extract_quoted(t) {
                if title.contains(needle) {
                    return match &submenu {
                        Some(s) => Some(format!("{}>{}", s.trim_end_matches('>'), title)),
                        None => Some(title),
                    };
                }
            }
        }
    }
    None
}

fn extract_quoted(line: &str) -> Option<String> {
    let start = line.find('\'')? + 1;
    let rest = &line[start..];
    let end = rest.find('\'')?;
    let title = &rest[..end];
    if title.is_empty() {
        None
    } else {
        Some(title.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sysctl_conf_content() {
        let c = sysctl_conf("bbr");
        assert!(c.contains("net.core.default_qdisc = fq"));
        assert!(c.contains("net.ipv4.tcp_congestion_control = bbr"));
    }

    fn sys_with(distro: &str, version: &str) -> SysInfo {
        SysInfo {
            distro: distro.into(),
            pretty: format!("{distro} {version}"),
            version: version.into(),
            pkg: PkgMgr::Apt,
            arch: "x86_64".into(),
            kernel: "6.12.0".into(),
            virt: "kvm".into(),
            is_container: false,
            current_cc: "cubic".into(),
            available_cc: "reno cubic bbr".into(),
            has_systemd: true,
        }
    }

    #[test]
    fn distro_too_new_guard() {
        // Debian 13 / 新 Ubuntu → 不换老内核
        assert!(distro_too_new_for_modkernel(&sys_with("debian", "13")));
        assert!(distro_too_new_for_modkernel(&sys_with("debian", "12")));
        assert!(distro_too_new_for_modkernel(&sys_with("ubuntu", "22.04")));
        assert!(distro_too_new_for_modkernel(&sys_with("ubuntu", "20.04")));
        // 老系统仍可走魔改内核
        assert!(!distro_too_new_for_modkernel(&sys_with("debian", "10")));
        assert!(!distro_too_new_for_modkernel(&sys_with("debian", "9")));
        assert!(!distro_too_new_for_modkernel(&sys_with("ubuntu", "18.04")));
        assert!(!distro_too_new_for_modkernel(&sys_with("centos", "7")));
    }

    #[test]
    fn kernel_ge_cases() {
        assert!(kernel_ge("4.9.0-1-amd64", 4, 9));
        assert!(kernel_ge("5.10.0-8-amd64", 4, 9));
        assert!(kernel_ge("4.11.8", 4, 9));
        assert!(kernel_ge("4.14.129-bbrplus", 4, 9));
        assert!(!kernel_ge("4.4.0-2-generic", 4, 9));
        assert!(!kernel_ge("3.16.0-4-amd64", 4, 9));
    }

    const DEBIAN_GRUB: &str = "\
menuentry 'Debian GNU/Linux' --class debian --class gnu-linux {
\tload_video
}
submenu 'Advanced options for Debian GNU/Linux' $menuentry_id_option 'gnulinux-advanced' {
\tmenuentry 'Debian GNU/Linux, with Linux 5.10.0-8-amd64' --class debian {
\t}
\tmenuentry 'Debian GNU/Linux, with Linux 4.11.8' --class debian {
\t}
}
menuentry 'System setup' {
}
";

    #[test]
    fn find_grub_entry_debian() {
        let e = find_grub_entry(DEBIAN_GRUB, "4.11.8").unwrap();
        assert_eq!(e, "Advanced options for Debian GNU/Linux>Debian GNU/Linux, with Linux 4.11.8");
        assert!(find_grub_entry(DEBIAN_GRUB, "6.99.1").is_none());
    }

    #[test]
    fn find_grub_entry_centos_flat() {
        let cfg = "\
menuentry 'CentOS Linux (3.10.0-1160.el7.x86_64) 7 (Core)' --class centos {
}
menuentry 'CentOS Linux (4.11.8) 7 (Core)' --class centos {
}
";
        let e = find_grub_entry(cfg, "4.11.8").unwrap();
        assert_eq!(e, "CentOS Linux (4.11.8) 7 (Core)");
    }

    #[test]
    fn replace_grub_default_variants() {
        assert_eq!(
            replace_grub_default("GRUB_DEFAULT=0\nGRUB_TIMEOUT=5\n", "A>B"),
            "GRUB_DEFAULT=\"A>B\"\nGRUB_TIMEOUT=5\n"
        );
        assert_eq!(
            replace_grub_default("#GRUB_DEFAULT=0\nGRUB_TIMEOUT=5\n", "A>B"),
            "#GRUB_DEFAULT=0\nGRUB_TIMEOUT=5\nGRUB_DEFAULT=\"A>B\"\n"
        );
        assert_eq!(replace_grub_default("", "A>B"), "GRUB_DEFAULT=\"A>B\"\n");
    }

    #[test]
    fn setup_and_makefile_content() {
        let s = setup_sh("tsunami");
        assert!(s.contains("VARIANT=\"tsunami\""));
        assert!(s.contains("4.11.8*"));
        assert!(s.contains("4.14.129-bbrplus*"));
        assert!(s.contains("insmod \"tcp_${VARIANT}.ko\""));

        let m = makefile("tsunami");
        assert!(m.contains("obj-m += tcp_tsunami.o"));
        assert!(m.contains("\tmake -C /lib/modules")); // 必须是 TAB
    }

    #[test]
    fn kernel_urls_build() {
        let mut sys = SysInfo {
            distro: "debian".into(),
            pretty: "Debian".into(),
            version: "9".into(),
            pkg: PkgMgr::Apt,
            arch: "x86_64".into(),
            kernel: "4.9.0".into(),
            virt: "kvm".into(),
            is_container: false,
            current_cc: "cubic".into(),
            available_cc: "reno cubic".into(),
            has_systemd: true,
        };
        let urls = kernel_urls(&sys, MOD_BBR_KERNEL);
        assert_eq!(urls.len(), 3);
        assert!(urls[0][0].ends_with("/bbr/debian-ubuntu/linux-headers-4.11.8-all.deb"));

        sys.distro = "centos".into();
        sys.version = "7".into();
        sys.pkg = PkgMgr::Yum;
        let urls = kernel_urls(&sys, BBRPLUS_KERNEL);
        assert_eq!(urls.len(), 1);
        assert!(urls[0][0].ends_with("/bbrplus/centos/7/kernel-4.14.129-bbrplus.rpm"));

        // CentOS 8 无预编译包
        sys.version = "8".into();
        assert!(kernel_urls(&sys, MOD_BBR_KERNEL).is_empty());
        // aarch64 无预编译包
        sys.version = "7".into();
        sys.arch = "aarch64".into();
        assert!(kernel_urls(&sys, MOD_BBR_KERNEL).is_empty());
    }
}
