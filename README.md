# sk5 — SOCKS5 代理 + 魔改 BBR 一键部署(Rust)

一套纯 Rust 实现的 VPS 代理部署方案(已在 **Debian 13** 上完整测试),包含两个部分:

| 组件 | 说明 |
|---|---|
| **sk5-server** | 轻量 SOCKS5 代理服务端,单文件实现、**零第三方依赖**,支持用户名密码认证(RFC 1929)、IPv4/IPv6/域名目标 |
| **sk5-install** | 一键安装器:自动检测系统 → 安装 BBR 加速(新系统自动用原生 BBR,老系统默认**魔改 BBR**,失败自动回退)→ 部署 sk5-server 并配置 systemd 开机自启 |

## 快速使用

### 方式一:在服务器上跑安装器(推荐先 --dry-run 预览)

```bash
# 默认:魔改 BBR + 随机端口 + 随机 16 位密码
sudo ./sk5-install

# 先看看会做什么,不实际执行
sudo ./sk5-install --dry-run

# 常用参数示例
sudo ./sk5-install --bbr modbbr --port 1080 --user myuser --pass '强密码'
sudo ./sk5-install --bbr native            # 只要原生 BBR(最稳)
sudo ./sk5-install --bbr bbrplus           # BBRPlus 内核
sudo ./sk5-install --bbr off               # 不动内核,只装代理
sudo ./sk5-install --uninstall             # 卸载(内核保留)
sudo ./sk5-install --status                # 查看 BBR / 服务 / 模块状态
```

安装完成会打印 `socks5://user:pass@ip:port` 连接地址、**Telegram 一键添加代理链接**(`tg://socks?...`);装了新内核会提示 `reboot`。

### 方式二:curl 一键安装(推荐)

```bash
curl -fsSL https://raw.githubusercontent.com/Mike09811/SOCKS5/main/install.sh | sudo bash
# 或带参数
curl -fsSL https://raw.githubusercontent.com/Mike09811/SOCKS5/main/install.sh | sudo bash -s -- --bbr native --port 1080
```

没有发布 Release 时,可手动上传编译好的 `sk5-server` 后执行:

```bash
sudo ./sk5-install --sk5-bin /root/sk5-server
```

## BBR 模式说明

安装器按系统自动选择:**Debian 11/12/13、Ubuntu 20.04+ 等新系统内核已自带 BBR,自动启用原生 BBR(立即生效,无需重启,不动内核)**;老系统才走魔改内核方案。也可手动指定:

| 模式 | 做法 | 生效方式 |
|---|---|---|
| `modbbr`(默认) | 安装 4.11.8 BBR 内核(经典 [cx9208/Linux-NetSpeed](https://github.com/cx9208/Linux-NetSpeed) 预编译包,多镜像回退),设为 GRUB 默认;部署 `sk5-bbrmod.service` 在**重启后**自动编译加载 `tcp_tsunami` 魔改算法模块 | 需重启 |
| `bbrplus` | 安装 4.14.129-bbrplus 内核(算法内置) | 需重启 |
| `native` | `modprobe tcp_bbr` + sysctl(fq + bbr) | 立即生效 |
| `off` | 跳过 | — |

魔改 BBR 的安全设计:

- **失败自动回退**:预编译包不存在 / 平台不支持 / 下载失败 → 自动转原生 BBR(`--no-fallback` 关闭该行为);
- **开机兜底**:重启后先保证原生 bbr 生效,`sk5-bbrmod` 服务再尝试编译模块(含 `KCFLAGS=-fno-pie` 重试);编译/加载失败则保持原生 bbr,不会让机器失联;
- 安装器会自动注释 `/etc/sysctl.conf` 里旧的拥塞控制配置,避免覆盖本次设置。

**适用范围**:魔改内核包只有 `x86_64` 版,支持 Debian 8-10、Ubuntu 14-18 与 CentOS 7;容器/OpenVZ 无法换内核。Debian 11+ / Ubuntu 20.04+ 这类新系统安装器会**自动跳过老内核、直接启用原生 BBR**(老内核在新系统上可能无法引导);在老系统上任何环节失败也会自动回退原生 BBR。

## sk5-server 单独使用

```bash
sk5-server --port 1080 --user u --pass p    # 命令行给了用户名/密码即开启认证
sk5-server --config /etc/sk5/sk5.conf       # 配置文件方式
sk5-server --no-auth                        # 无认证(务必配合防火墙限制来源)
```

配置文件格式(`key = value`,`#` 注释):

```ini
listen = 0.0.0.0
port   = 1080
auth   = userpass        # none / userpass
user   = myuser
pass   = mypassword
```

客户端:`curl --socks5-hostname u:p@host:1080 http://example.com`,或任何支持 SOCKS5 的软件。

## 构建与发布

本机开发/测试(测试含 SOCKS5 协议回环集成测试):

```bash
cargo test --workspace
```

服务器是 Linux,在 macOS/Windows 上需要交叉编译或走 CI:

```bash
rustup target add x86_64-unknown-linux-musl
cargo install cross            # 依赖 Docker
cross build --release --workspace --target x86_64-unknown-linux-musl
```

**推荐:推送 tag 自动发布**。仓库自带 `.github/workflows/release.yml`:

1. 仓库地址已内置(`Mike09811/SOCKS5`,在 `installer/src/sk5.rs` 与 `install.sh` 两处,换仓库时改这两处即可);
2. `git tag v0.1.x && git push --tags`,CI 会用 musl 静态编译 x86_64/aarch64 两个架构的 `sk5-server-*` 与 `sk5-install-*` 并挂到 Release。

之后 `install.sh` / `sk5-install` 默认下载地址即可直接工作,也可用 `--bin-url` 或环境变量 `SK5_BIN_URL` 覆盖。

## 安全提醒

- 默认随机端口 + 随机密码,避免被扫描爆破;`--no-auth` 慎用;
- 云厂商**安全组**需放行对应 TCP 端口(安装器只能处理本机 ufw/firewalld);
- sk5-server 以 systemd 沙箱运行(`ProtectSystem=strict` 等加固,仅保留绑定低端口的能力)。

## 目录结构

```
├── sk5/               # SOCKS5 服务端(零依赖)
│   └── src/main.rs
├── installer/         # 一键安装器(仅依赖 ureq 做 HTTPS 下载)
│   └── src/           #   cli / sys / exec / net / bbr / sk5 / log / util
├── install.sh         # curl | bash 引导脚本
└── .github/workflows/release.yml
```
