//! 命令行参数解析(无 clap,手工实现,保持依赖最小)。

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BbrMode {
    /// 原生 BBR(内核 >= 4.9,仅 sysctl 启用)
    Native,
    /// 魔改 BBR:更换 4.11.8 内核 + 编译加载 tcp_tsunami/nanqinlang 模块
    ModBbr,
    /// BBRPlus:更换 4.14.129-bbrplus 内核
    BbrPlus,
    /// 跳过 BBR
    Off,
}

impl BbrMode {
    pub fn label(&self) -> &'static str {
        match self {
            BbrMode::Native => "原生 BBR",
            BbrMode::ModBbr => "魔改 BBR",
            BbrMode::BbrPlus => "BBRPlus",
            BbrMode::Off => "不安装",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ModVariant {
    Tsunami,
    Nanqinlang,
}

#[derive(Clone, Debug)]
pub struct Args {
    pub bbr: BbrMode,
    pub bbrmod_variant: ModVariant,
    /// 魔改 BBR 失败时不回退原生 BBR
    pub no_fallback: bool,
    pub port: Option<u16>,
    pub user: Option<String>,
    pub pass: Option<String>,
    pub no_auth: bool,
    /// sk5-server 二进制来源:本地路径或 http(s) URL;缺省从 Release 下载
    pub sk5_bin: Option<String>,
    /// 覆盖默认的 sk5-server 下载地址模板
    pub bin_url: Option<String>,
    pub yes: bool,
    pub dry_run: bool,
    pub uninstall: bool,
    pub status: bool,
    pub no_firewall: bool,
}

impl Default for Args {
    fn default() -> Self {
        Args {
            bbr: BbrMode::ModBbr,
            bbrmod_variant: ModVariant::Tsunami,
            no_fallback: false,
            port: None,
            user: None,
            pass: None,
            no_auth: false,
            sk5_bin: None,
            bin_url: None,
            yes: false,
            dry_run: false,
            uninstall: false,
            status: false,
            no_firewall: false,
        }
    }
}

/// 执行上下文:dry-run 时只打印动作不落盘。
pub struct Ctx {
    pub dry_run: bool,
}

pub const HELP: &str = "\
sk5-install — SOCKS5 代理 + BBR 加速一键安装器

用法: sk5-install [选项]

BBR 相关:
  --bbr <mode>        BBR 模式,默认 modbbr
                        modbbr   魔改 BBR(4.11.8 内核 + tsunami 模块,经典魔改方案)
                        bbrplus  BBRPlus 内核(4.14.129-bbrplus)
                        native   原生 BBR(仅 sysctl 启用,内核需 >= 4.9)
                        off      跳过 BBR
  --bbrmod-variant <v>  魔改算法变体:tsunami(默认) | nanqinlang
  --no-fallback         魔改 BBR 安装失败时不回退到原生 BBR

sk5 服务相关:
  --port <N>           SOCKS5 监听端口(默认随机 20000-60000)
  --user <NAME>        认证用户名(默认 sk5)
  --pass <PASS>        认证密码(默认随机 16 位)
  --no-auth            关闭认证(公开代理,危险)
  --sk5-bin <PATH|URL> sk5-server 二进制的本地路径或下载地址
  --bin-url <URL>      覆盖默认下载地址模板
  --no-firewall        不自动配置 ufw/firewalld 放行

其他:
  --yes                跳过交互确认
  --dry-run            只打印将执行的动作,不做任何修改
  --uninstall          卸载 sk5(内核与 sysctl 保留)
  --status             查看当前 BBR 与服务状态
  -h, --help           显示本帮助
  -V, --version        显示版本

示例:
  sk5-install                          # 全默认:魔改 BBR + 随机端口随机密码
  sk5-install --bbr native --port 1080 # 原生 BBR + 指定端口
  sk5-install --sk5-bin ./sk5-server   # 使用本地上传的二进制
";

fn next_val<I: Iterator<Item = String>>(it: &mut I, name: &str) -> Result<String, String> {
    it.next().ok_or_else(|| format!("{name} 需要一个值"))
}

pub fn parse<I>(mut it: I) -> Result<Args, String>
where
    I: Iterator<Item = String>,
{
    let mut a = Args::default();
    while let Some(tok) = it.next() {
        match tok.as_str() {
            "--bbr" => {
                a.bbr = match next_val(&mut it, "--bbr")?.to_ascii_lowercase().as_str() {
                    "native" | "bbr" => BbrMode::Native,
                    "modbbr" | "mod" | "magic" | "魔改" => BbrMode::ModBbr,
                    "bbrplus" | "plus" => BbrMode::BbrPlus,
                    "off" | "none" | "no" => BbrMode::Off,
                    other => return Err(format!("未知 BBR 模式: {other}(native/modbbr/bbrplus/off)")),
                };
            }
            "--bbrmod-variant" => {
                a.bbrmod_variant = match next_val(&mut it, "--bbrmod-variant")?.to_ascii_lowercase().as_str() {
                    "tsunami" => ModVariant::Tsunami,
                    "nanqinlang" => ModVariant::Nanqinlang,
                    other => return Err(format!("未知魔改变体: {other}(tsunami/nanqinlang)")),
                };
            }
            "--no-fallback" => a.no_fallback = true,
            "--port" => {
                let v = next_val(&mut it, "--port")?;
                a.port = Some(v.parse::<u16>().map_err(|_| format!("端口非法: {v}"))?);
            }
            "--user" => a.user = Some(next_val(&mut it, "--user")?),
            "--pass" => a.pass = Some(next_val(&mut it, "--pass")?),
            "--no-auth" => a.no_auth = true,
            "--sk5-bin" => a.sk5_bin = Some(next_val(&mut it, "--sk5-bin")?),
            "--bin-url" => a.bin_url = Some(next_val(&mut it, "--bin-url")?),
            "--yes" | "-y" => a.yes = true,
            "--dry-run" => a.dry_run = true,
            "--uninstall" => a.uninstall = true,
            "--status" => a.status = true,
            "--no-firewall" => a.no_firewall = true,
            "-h" | "--help" => return Err(String::new()), // 空错误 = 显示帮助
            "-V" | "--version" => return Err("\u{0}version".to_string()),
            other => return Err(format!("未知参数: {other}")),
        }
    }
    Ok(a)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Result<Args, String> {
        parse(v.iter().map(|s| s.to_string()))
    }

    #[test]
    fn defaults() {
        let a = args(&[]).unwrap();
        assert_eq!(a.bbr, BbrMode::ModBbr);
        assert_eq!(a.bbrmod_variant, ModVariant::Tsunami);
        assert!(!a.yes && !a.dry_run && !a.uninstall && !a.status && !a.no_auth);
    }

    #[test]
    fn parses_all_flags() {
        let a = args(&[
            "--bbr", "native",
            "--port", "1080",
            "--user", "u",
            "--pass", "p",
            "--yes",
            "--dry-run",
            "--sk5-bin", "/tmp/sk5-server",
        ])
        .unwrap();
        assert_eq!(a.bbr, BbrMode::Native);
        assert_eq!(a.port, Some(1080));
        assert_eq!(a.user.as_deref(), Some("u"));
        assert_eq!(a.pass.as_deref(), Some("p"));
        assert!(a.yes && a.dry_run);
        assert_eq!(a.sk5_bin.as_deref(), Some("/tmp/sk5-server"));
    }

    #[test]
    fn bbr_alias_and_variant() {
        let a = args(&["--bbr", "plus", "--bbrmod-variant", "nanqinlang"]).unwrap();
        assert_eq!(a.bbr, BbrMode::BbrPlus);
        assert_eq!(a.bbrmod_variant, ModVariant::Nanqinlang);
    }

    #[test]
    fn rejects_unknown_and_missing_value() {
        assert!(args(&["--wat"]).is_err());
        assert!(args(&["--bbr"]).is_err());
        assert!(args(&["--bbr", "xxx"]).is_err());
        assert!(args(&["--port", "abc"]).is_err());
    }
}
