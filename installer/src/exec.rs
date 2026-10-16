//! 命令执行与文件写入(贯穿 dry-run 语义)。

use crate::cli::Ctx;
use std::path::Path;
use std::process::Command;

pub struct CmdFail {
    pub cmd: String,
    pub stderr: String,
}

impl std::fmt::Display for CmdFail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.stderr.is_empty() {
            write!(f, "命令执行失败: {}", self.cmd)
        } else {
            let tail = if self.stderr.len() > 800 {
                format!("…{}", &self.stderr[self.stderr.len() - 800..])
            } else {
                self.stderr.clone()
            };
            write!(f, "命令执行失败: {}\n{}", self.cmd, tail)
        }
    }
}

/// 执行命令并要求成功(受 dry-run 控制:dry-run 只打印)。
pub fn run(ctx: &Ctx, prog: &str, args: &[&str]) -> Result<String, CmdFail> {
    let cmdline = if args.is_empty() {
        prog.to_string()
    } else {
        format!("{} {}", prog, args.join(" "))
    };
    if ctx.dry_run {
        crate::log::plain(format!("  [dry-run] $ {cmdline}"));
        return Ok(String::new());
    }
    let out = match Command::new(prog)
        .args(args)
        .env("DEBIAN_FRONTEND", "noninteractive")
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            return Err(CmdFail {
                cmd: cmdline,
                stderr: format!("无法启动 {prog}: {e}"),
            })
        }
    };
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if out.status.success() {
        Ok(stdout)
    } else {
        Err(CmdFail {
            cmd: cmdline,
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        })
    }
}

/// 执行命令,失败仅提示并返回空串。
pub fn run_soft(ctx: &Ctx, prog: &str, args: &[&str]) -> String {
    match run(ctx, prog, args) {
        Ok(s) => s,
        Err(e) => {
            crate::log::plain(format!("  (跳过) {e}"));
            String::new()
        }
    }
}

/// 只读探测命令:永远真实执行,不受 dry-run 影响(用于环境检测)。
pub fn probe(prog: &str, args: &[&str]) -> String {
    let out = Command::new(prog).args(args).output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => String::new(),
    }
}

/// `command -v prog` 是否存在。
pub fn have(prog: &str) -> bool {
    !probe("sh", &["-c", &format!("command -v {prog}")]).is_empty()
}

/// 写文件(自动创建父目录,可选权限);dry-run 只打印。
pub fn write_file(ctx: &Ctx, path: &str, content: &str, mode: Option<u32>) -> Result<(), String> {
    if ctx.dry_run {
        crate::log::plain(format!("  [dry-run] 写入 {path}({} 字节)", content.len()));
        return Ok(());
    }
    if let Some(p) = Path::new(path).parent() {
        std::fs::create_dir_all(p).map_err(|e| format!("创建目录失败 {}: {e}", p.display()))?;
    }
    std::fs::write(path, content).map_err(|e| format!("写入 {path} 失败: {e}"))?;
    if let Some(m) = mode {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(m))
                .map_err(|e| format!("设置权限 {path} 失败: {e}"))?;
        }
        #[cfg(not(unix))]
        let _ = m;
    }
    Ok(())
}

/// 删除文件或目录(忽略不存在);dry-run 只打印。
pub fn remove_path(ctx: &Ctx, path: &str) {
    if ctx.dry_run {
        crate::log::plain(format!("  [dry-run] 删除 {path}"));
        return;
    }
    if Path::new(path).is_dir() {
        let _ = std::fs::remove_dir_all(path);
    } else {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_echo() {
        assert_eq!(probe("echo", &["hello"]), "hello");
    }

    #[test]
    fn have_sh() {
        assert!(have("sh"));
        assert!(!have("__definitely_not_exists__"));
    }

    #[test]
    fn write_file_respects_dry_run() {
        let ctx = Ctx { dry_run: true };
        let p = "/tmp/sk5-test-should-not-exist.conf";
        write_file(&ctx, p, "hello", None).unwrap();
        assert!(!Path::new(p).exists());
    }

    #[test]
    fn write_file_real() {
        let ctx = Ctx { dry_run: false };
        let p = "/tmp/sk5-test-write.conf";
        write_file(&ctx, p, "hello", Some(0o600)).unwrap();
        assert_eq!(std::fs::read_to_string(p).unwrap(), "hello");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        remove_path(&ctx, p);
        assert!(!Path::new(p).exists());
    }
}
