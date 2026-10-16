//! sk5-server — 轻量 SOCKS5 代理服务端,单文件、零第三方依赖。
//! 支持:CONNECT 命令、无认证/用户名密码认证(RFC 1928 / RFC 1929)、
//! IPv4 / IPv6 / 域名目标地址、域名解析与多地址回退连接。

use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::net::{
    IpAddr, Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs,
};
use std::process;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const BUF_SIZE: usize = 64 * 1024;
/// 单条连接读/写超时(空闲连接会被回收)
const IO_TIMEOUT: Duration = Duration::from_secs(600);
/// 连接上游目标的最长等待
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_USER_LEN: usize = 64;
const MAX_PASS_LEN: usize = 128;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AuthMode {
    None,
    UserPass,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub listen: String,
    pub port: u16,
    pub auth: AuthMode,
    pub user: String,
    pub pass: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            listen: "0.0.0.0".to_string(),
            port: 1080,
            auth: AuthMode::None,
            user: String::new(),
            pass: String::new(),
        }
    }
}

fn log(msg: impl AsRef<str>) {
    println!("[sk5] {}", msg.as_ref());
}

/// 解析 `key = value` 格式配置;返回 (配置, 警告列表)。
pub fn parse_conf(text: &str) -> (Config, Vec<String>) {
    let mut cfg = Config::default();
    let mut warnings = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            warnings.push(format!("第{}行格式错误(应为 key = value): {}", i + 1, raw));
            continue;
        };
        let (k, v) = (k.trim(), v.trim());
        match k {
            "listen" => cfg.listen = v.to_string(),
            "port" => match v.parse::<u16>() {
                Ok(p) => cfg.port = p,
                Err(_) => warnings.push(format!("第{}行 port 非法: {}", i + 1, v)),
            },
            "auth" => {
                cfg.auth = match v {
                    "none" | "off" | "false" => AuthMode::None,
                    "userpass" | "password" | "on" | "true" => AuthMode::UserPass,
                    other => {
                        warnings.push(format!(
                            "第{}行 auth 取值不支持: {}(支持 none / userpass)",
                            i + 1,
                            other
                        ));
                        cfg.auth
                    }
                };
            }
            "user" => cfg.user = v.to_string(),
            "pass" => cfg.pass = v.to_string(),
            other => warnings.push(format!("第{}行未知配置项: {}", i + 1, other)),
        }
    }
    (cfg, warnings)
}

fn usage() {
    println!(
        "sk5-server {VERSION} — 轻量 SOCKS5 代理服务端

用法:
  sk5-server [选项]

选项:
  --config <PATH>   配置文件路径(默认 /etc/sk5/sk5.conf,不存在则用默认值)
  --port <N>        覆盖配置中的监听端口
  --user <NAME>     覆盖配置中的用户名
  --pass <PASS>     覆盖配置中的密码
  --no-auth         强制关闭认证(公开代理,注意安全)
  -h, --help        显示帮助
  -V, --version     显示版本

配置文件格式(key = value,# 为注释):
  listen = 0.0.0.0     # 监听地址
  port   = 1080        # 监听端口
  auth   = userpass    # none 或 userpass
  user   = xxx
  pass   = yyy"
    );
}

fn main() {
    let mut conf_path = String::from("/etc/sk5/sk5.conf");
    let mut over_port: Option<u16> = None;
    let mut over_user: Option<String> = None;
    let mut over_pass: Option<String> = None;
    let mut no_auth = false;

    let mut it = env::args().skip(1);
    while let Some(a) = it.next() {
        let mut value = |name: &str| -> Result<String, String> {
            it.next().ok_or_else(|| format!("{name} 需要一个值"))
        };
        match a.as_str() {
            "--config" => match value("--config") {
                Ok(v) => conf_path = v,
                Err(e) => {
                    eprintln!("{e}");
                    process::exit(2);
                }
            },
            "--port" => match value("--port") {
                Ok(v) => match v.parse::<u16>() {
                    Ok(p) => over_port = Some(p),
                    Err(_) => {
                        eprintln!("端口非法: {v}");
                        process::exit(2);
                    }
                },
                Err(e) => {
                    eprintln!("{e}");
                    process::exit(2);
                }
            },
            "--user" => match value("--user") {
                Ok(v) => over_user = Some(v),
                Err(e) => {
                    eprintln!("{e}");
                    process::exit(2);
                }
            },
            "--pass" => match value("--pass") {
                Ok(v) => over_pass = Some(v),
                Err(e) => {
                    eprintln!("{e}");
                    process::exit(2);
                }
            },
            "--no-auth" => no_auth = true,
            "-h" | "--help" => {
                usage();
                return;
            }
            "-V" | "--version" => {
                println!("sk5-server {VERSION}");
                return;
            }
            other => {
                eprintln!("未知参数: {other}(--help 查看用法)");
                process::exit(2);
            }
        }
    }

    let (mut cfg, warnings) = match fs::read_to_string(&conf_path) {
        Ok(t) => parse_conf(&t),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            (Config::default(), vec![format!("配置文件 {conf_path} 不存在,使用默认值")])
        }
        Err(e) => {
            eprintln!("[sk5] 读取配置失败: {e}");
            process::exit(1);
        }
    };
    for w in &warnings {
        eprintln!("[sk5] 配置警告: {w}");
    }
    if let Some(p) = over_port {
        cfg.port = p;
    }
    if let Some(ref u) = over_user {
        cfg.user = u.clone();
    }
    if let Some(ref p) = over_pass {
        cfg.pass = p.clone();
    }
    // 命令行显式给了用户名或密码,即视为要求开启认证
    if over_user.is_some() || over_pass.is_some() {
        cfg.auth = AuthMode::UserPass;
    }
    if no_auth {
        cfg.auth = AuthMode::None;
    }
    if cfg.auth == AuthMode::UserPass && (cfg.user.is_empty() || cfg.pass.is_empty()) {
        eprintln!("[sk5] 错误: auth = userpass 但 user/pass 为空");
        process::exit(1);
    }

    let listener = match TcpListener::bind((cfg.listen.as_str(), cfg.port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("[sk5] 监听 {}:{} 失败: {e}", cfg.listen, cfg.port);
            process::exit(1);
        }
    };
    let real_port = listener.local_addr().map(|a| a.port()).unwrap_or(cfg.port);
    log(format!("sk5-server v{VERSION} 启动"));
    log(format!("监听 {}:{}", cfg.listen, real_port));
    match cfg.auth {
        AuthMode::None => log("认证: 无(公开代理,请确保防火墙/安全组限制来源)"),
        AuthMode::UserPass => log(format!("认证: 用户名密码(user = {})", cfg.user)),
    }
    serve(listener, cfg);
}

fn serve(listener: TcpListener, cfg: Config) {
    let cfg = Arc::new(cfg);
    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let cfg = cfg.clone();
                thread::spawn(move || handle_client(s, &cfg));
            }
            Err(e) => eprintln!("[sk5] accept 失败: {e}"),
        }
    }
}

fn handle_client(mut cli: TcpStream, cfg: &Config) {
    let peer = cli
        .peer_addr()
        .map(|a| a.to_string())
        .unwrap_or_else(|_| "?".to_string());
    let _ = cli.set_read_timeout(Some(IO_TIMEOUT));
    let _ = cli.set_write_timeout(Some(IO_TIMEOUT));
    if let Err(e) = socks5(&mut cli, cfg, &peer) {
        log(format!("{peer} 会话结束: {e}"));
    }
}

/// 从套接字精确读取 n 字节,错误带上下文。
fn rd(s: &mut TcpStream, n: usize, what: &str) -> Result<Vec<u8>, String> {
    let mut buf = vec![0u8; n];
    s.read_exact(&mut buf)
        .map_err(|e| format!("读取{what}失败: {e}"))?;
    Ok(buf)
}

fn reply(s: &mut TcpStream, rep: u8) -> Result<(), String> {
    // BND.ADDR = 0.0.0.0, BND.PORT = 0
    s.write_all(&[0x05, rep, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
        .map_err(|e| format!("发送响应失败: {e}"))
}

/// 常数时间比较,避免认证时序侧信道。
fn const_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

fn socks5(cli: &mut TcpStream, cfg: &Config, peer: &str) -> Result<(), String> {
    // ---- 1. 方法协商 ----
    let hdr = rd(cli, 2, "协商头")?;
    if hdr[0] != 0x05 {
        return Err("不是 SOCKS5 协议".into());
    }
    let nmethods = hdr[1] as usize;
    if nmethods == 0 || nmethods > 16 {
        return Err("METHOD 数量非法".into());
    }
    let methods = rd(cli, nmethods, "METHOD 列表")?;
    match cfg.auth {
        AuthMode::None => {
            if !methods.contains(&0x00) {
                let _ = cli.write_all(&[0x05, 0xFF]);
                return Err("客户端不支持无认证方式".into());
            }
            cli.write_all(&[0x05, 0x00]).map_err(|e| format!("发送响应失败: {e}"))?;
        }
        AuthMode::UserPass => {
            if !methods.contains(&0x02) {
                let _ = cli.write_all(&[0x05, 0xFF]);
                return Err("客户端不支持用户名密码认证".into());
            }
            cli.write_all(&[0x05, 0x02]).map_err(|e| format!("发送响应失败: {e}"))?;
            // RFC 1929 子协商
            let vh = rd(cli, 2, "认证版本")?;
            if vh[0] != 0x01 {
                return Err("认证子协议版本错误".into());
            }
            let ulen = vh[1] as usize;
            if ulen == 0 || ulen > MAX_USER_LEN {
                return Err("用户名长度非法".into());
            }
            let user = String::from_utf8_lossy(&rd(cli, ulen, "用户名")?).into_owned();
            let pl = rd(cli, 1, "密码长度")?;
            let plen = pl[0] as usize;
            if plen > MAX_PASS_LEN {
                return Err("密码长度非法".into());
            }
            let pass = String::from_utf8_lossy(&rd(cli, plen, "密码")?).into_owned();
            let ok = const_eq(user.as_bytes(), cfg.user.as_bytes())
                && const_eq(pass.as_bytes(), cfg.pass.as_bytes());
            if !ok {
                let _ = cli.write_all(&[0x05, 0x01]);
                return Err("认证失败(用户名或密码错误)".into());
            }
            cli.write_all(&[0x05, 0x00]).map_err(|e| format!("发送响应失败: {e}"))?;
        }
    }

    // ---- 2. 请求 ----
    let rh = rd(cli, 4, "请求头")?;
    if rh[0] != 0x05 {
        return Err("请求版本错误".into());
    }
    match rh[1] {
        0x01 => {}
        0x03 => {
            reply(cli, 0x07)?;
            return Err("不支持 UDP ASSOCIATE".into());
        }
        _ => {
            reply(cli, 0x07)?;
            return Err("不支持该命令(仅支持 CONNECT)".into());
        }
    }
    let host: String = match rh[3] {
        0x01 => {
            let b = rd(cli, 4, "IPv4 地址")?;
            IpAddr::V4(Ipv4Addr::new(b[0], b[1], b[2], b[3])).to_string()
        }
        0x03 => {
            let l = rd(cli, 1, "域名长度")?;
            let len = l[0] as usize;
            if len == 0 {
                return Err("域名为空".into());
            }
            let d = rd(cli, len, "域名")?;
            String::from_utf8_lossy(&d).into_owned()
        }
        0x04 => {
            let b = rd(cli, 16, "IPv6 地址")?;
            let mut a = [0u8; 16];
            a.copy_from_slice(&b);
            IpAddr::V6(Ipv6Addr::from(a)).to_string()
        }
        _ => {
            reply(cli, 0x08)?;
            return Err("地址类型非法".into());
        }
    };
    let pb = rd(cli, 2, "端口")?;
    let port = u16::from_be_bytes([pb[0], pb[1]]);
    if port == 0 {
        reply(cli, 0x01)?;
        return Err("目标端口非法".into());
    }

    // ---- 3. 连接目标 ----
    let mut up = match connect_target(&host, port) {
        Ok(s) => s,
        Err(e) => {
            let rep = if e.kind() == io::ErrorKind::ConnectionRefused { 0x05 } else { 0x04 };
            reply(cli, rep)?;
            return Err(format!("连接 {host}:{port} 失败: {e}"));
        }
    };
    reply(cli, 0x00)?;
    log(format!("{peer} -> {host}:{port} 已连接"));

    // ---- 4. 双向转发 ----
    relay(cli, &mut up);
    Ok(())
}

fn connect_target(host: &str, port: u16) -> io::Result<TcpStream> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return TcpStream::connect_timeout(&SocketAddr::new(ip, port), CONNECT_TIMEOUT);
    }
    let addrs: Vec<SocketAddr> = (host, port)
        .to_socket_addrs()?
        .filter(|a| a.is_ipv4())
        .chain((host, port).to_socket_addrs()?.filter(|a| a.is_ipv6()))
        .collect();
    let mut last = io::Error::new(io::ErrorKind::AddrNotAvailable, "域名未解析出地址");
    for a in addrs {
        match TcpStream::connect_timeout(&a, CONNECT_TIMEOUT) {
            Ok(s) => return Ok(s),
            Err(e) => last = e,
        }
    }
    Err(last)
}

fn relay(cli: &mut TcpStream, up: &mut TcpStream) {
    let ca = match cli.try_clone() {
        Ok(c) => c,
        Err(_) => return,
    };
    let cb = match up.try_clone() {
        Ok(c) => c,
        Err(_) => return,
    };
    // cli -> up 在子线程;up -> cli 在当前线程
    let t = thread::spawn(move || {
        let mut ca = ca;
        let mut cb = cb;
        copy_half(&mut ca, &mut cb);
        let _ = cb.shutdown(Shutdown::Write);
    });
    copy_half(up, cli);
    let _ = cli.shutdown(Shutdown::Write);
    let _ = t.join();
}

fn copy_half(src: &mut TcpStream, dst: &mut TcpStream) -> u64 {
    let mut buf = [0u8; BUF_SIZE];
    let mut total = 0u64;
    loop {
        match src.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if dst.write_all(&buf[..n]).is_err() {
                    break;
                }
                total += n as u64;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    total
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    fn test_cfg(auth: AuthMode) -> Config {
        Config {
            listen: "127.0.0.1".to_string(),
            port: 0,
            auth,
            user: "u1".to_string(),
            pass: "p1".to_string(),
        }
    }

    fn start_server(cfg: Config) -> SocketAddr {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        thread::spawn(move || serve(l, cfg));
        addr
    }

    fn start_echo() -> SocketAddr {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        thread::spawn(move || {
            for c in l.incoming().flatten() {
                thread::spawn(move || {
                    let mut c = c;
                    let mut b = [0u8; 1024];
                    loop {
                        match c.read(&mut b) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                if c.write_all(&b[..n]).is_err() {
                                    break;
                                }
                            }
                        }
                    }
                });
            }
        });
        addr
    }

    fn read_n(s: &mut TcpStream, n: usize) -> Vec<u8> {
        let mut v = vec![0u8; n];
        s.read_exact(&mut v).unwrap();
        v
    }

    #[test]
    fn parse_conf_basics() {
        let (cfg, w) = parse_conf(
            "# 注释\nlisten = 127.0.0.1\nport = 18080\nauth = userpass\nuser = a\npass = b\nfoo = 1\n",
        );
        assert_eq!(cfg.listen, "127.0.0.1");
        assert_eq!(cfg.port, 18080);
        assert_eq!(cfg.auth, AuthMode::UserPass);
        assert_eq!(cfg.user, "a");
        assert_eq!(cfg.pass, "b");
        assert_eq!(w.len(), 1); // 未知配置项 foo
    }

    #[test]
    fn parse_conf_defaults() {
        let (cfg, w) = parse_conf("");
        assert_eq!(cfg.port, 1080);
        assert_eq!(cfg.auth, AuthMode::None);
        assert!(w.is_empty());
    }

    #[test]
    fn const_eq_works() {
        assert!(const_eq(b"abc", b"abc"));
        assert!(!const_eq(b"abc", b"abd"));
        assert!(!const_eq(b"abc", b"ab"));
    }

    #[test]
    fn socks5_userpass_roundtrip() {
        let echo = start_echo();
        let srv = start_server(test_cfg(AuthMode::UserPass));
        let mut c = TcpStream::connect(srv).unwrap();
        // 协商:仅支持用户名密码
        c.write_all(&[5, 1, 2]).unwrap();
        assert_eq!(read_n(&mut c, 2), vec![5, 2]);
        c.write_all(&[1, 2, b'u', b'1', 2, b'p', b'1']).unwrap();
        assert_eq!(read_n(&mut c, 2), vec![5, 0]);
        let mut req = vec![5, 1, 0, 1];
        req.extend_from_slice(&echo.ip().to_string().parse::<Ipv4Addr>().unwrap().octets());
        req.extend_from_slice(&echo.port().to_be_bytes());
        c.write_all(&req).unwrap();
        let rep = read_n(&mut c, 10);
        assert_eq!(rep[1], 0, "CONNECT 应成功");
        c.write_all(b"hello sk5").unwrap();
        let mut buf = [0u8; 9];
        c.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"hello sk5");
    }

    #[test]
    fn socks5_wrong_password_rejected() {
        let _echo = start_echo();
        let srv = start_server(test_cfg(AuthMode::UserPass));
        let mut c = TcpStream::connect(srv).unwrap();
        c.write_all(&[5, 1, 2]).unwrap();
        assert_eq!(read_n(&mut c, 2), vec![5, 2]);
        c.write_all(&[1, 2, b'u', b'1', 2, b'x', b'x']).unwrap();
        assert_eq!(read_n(&mut c, 2), vec![5, 1], "密码错误应返回 0x01");
    }

    #[test]
    fn socks5_noauth_roundtrip() {
        let echo = start_echo();
        let srv = start_server(test_cfg(AuthMode::None));
        let mut c = TcpStream::connect(srv).unwrap();
        c.write_all(&[5, 1, 0]).unwrap();
        assert_eq!(read_n(&mut c, 2), vec![5, 0]);
        let mut req = vec![5, 1, 0, 3, 9];
        req.extend_from_slice(b"127.0.0.1");
        req.extend_from_slice(&echo.port().to_be_bytes());
        c.write_all(&req).unwrap();
        let rep = read_n(&mut c, 10);
        assert_eq!(rep[1], 0);
        c.write_all(b"ping").unwrap();
        let mut buf = [0u8; 4];
        c.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"ping");
    }
}
