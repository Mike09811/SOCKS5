//! 系统环境检测:发行版、包管理器、架构、内核、虚拟化、拥塞控制状态。

use crate::exec;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PkgMgr {
    Apt,
    Dnf,
    Yum,
    Apk,
    Unknown,
}

#[derive(Clone, Debug)]
pub struct SysInfo {
    /// debian / ubuntu / centos / rhel / alpine / …(os-release 的 ID)
    pub distro: String,
    pub pretty: String,
    pub version: String,
    pub pkg: PkgMgr,
    pub arch: String,
    /// uname -r,如 5.10.0-8-amd64
    pub kernel: String,
    pub virt: String,
    /// 容器/OpenVZ 等共享内核环境,无法更换内核
    pub is_container: bool,
    /// sysctl -n net.ipv4.tcp_congestion_control
    pub current_cc: String,
    /// /proc/sys/net/ipv4/tcp_available_congestion_control
    pub available_cc: String,
    pub has_systemd: bool,
}

pub fn parse_os_release(text: &str) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    for line in text.lines() {
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = l.split_once('=') {
            let v = v.trim().trim_matches('"').trim_matches('\'').to_string();
            m.insert(k.trim().to_string(), v);
        }
    }
    m
}

pub fn detect() -> SysInfo {
    let osr = std::fs::read_to_string("/etc/os-release")
        .or_else(|_| std::fs::read_to_string("/usr/lib/os-release"))
        .unwrap_or_default();
    let m = parse_os_release(&osr);

    let pkg = if exec::have("apt-get") {
        PkgMgr::Apt
    } else if exec::have("dnf") {
        PkgMgr::Dnf
    } else if exec::have("yum") {
        PkgMgr::Yum
    } else if exec::have("apk") {
        PkgMgr::Apk
    } else {
        PkgMgr::Unknown
    };

    let mut distro = m.get("ID").cloned().unwrap_or_default();
    if distro.is_empty() {
        distro = match pkg {
            PkgMgr::Apt => "debian",
            PkgMgr::Dnf | PkgMgr::Yum => "centos",
            PkgMgr::Apk => "alpine",
            PkgMgr::Unknown => "",
        }
        .to_string();
    }

    let virt = exec::probe("systemd-detect-virt", &[]);
    let container_virt = exec::probe("systemd-detect-virt", &["--container"]);
    let is_container = (!container_virt.is_empty() && container_virt != "none")
        || virt == "openvz"
        || Path::new("/proc/vz").exists();

    SysInfo {
        distro: distro.clone(),
        pretty: m
            .get("PRETTY_NAME")
            .cloned()
            .unwrap_or_else(|| if distro.is_empty() { "未知系统".into() } else { distro.clone() }),
        version: m.get("VERSION_ID").cloned().unwrap_or_default(),
        pkg,
        arch: exec::probe("uname", &["-m"]),
        kernel: exec::probe("uname", &["-r"]),
        virt,
        is_container,
        current_cc: current_cc(),
        available_cc: std::fs::read_to_string("/proc/sys/net/ipv4/tcp_available_congestion_control")
            .unwrap_or_default()
            .trim()
            .to_string(),
        has_systemd: Path::new("/run/systemd/system").exists(),
    }
}

/// 当前拥塞控制算法:直接读 /proc,不依赖 sysctl 命令(procps)。
pub fn current_cc() -> String {
    std::fs::read_to_string("/proc/sys/net/ipv4/tcp_congestion_control")
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// 当前队列规则(同上,读 /proc)。
pub fn current_qdisc() -> String {
    std::fs::read_to_string("/proc/sys/net/core/default_qdisc")
        .unwrap_or_default()
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_os_release_sample() {
        let m = parse_os_release(
            "NAME=\"Debian GNU/Linux\"\nVERSION_ID=\"12\"\nID=debian\nPRETTY_NAME=\"Debian GNU/Linux 12 (bookworm)\"\n# 注释\n",
        );
        assert_eq!(m.get("ID").unwrap(), "debian");
        assert_eq!(m.get("VERSION_ID").unwrap(), "12");
        assert_eq!(m.get("PRETTY_NAME").unwrap(), "Debian GNU/Linux 12 (bookworm)");
        assert!(!m.contains_key("NAME2"));
    }

    #[test]
    fn detect_runs_anywhere() {
        // 在 macOS 开发机上也能跑,只是字段为空
        let s = detect();
        assert!(!s.arch.is_empty()); // uname -m 到处都有
    }
}
