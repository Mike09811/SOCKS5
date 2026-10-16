//! HTTPS 下载(ureq + rustls,无需系统 curl/openssl),支持多镜像回退。

use crate::cli::Ctx;
use std::fs::File;
use std::io::{Read, Write};
use std::time::Duration;

/// 依次尝试多个镜像下载同一文件,返回最终使用的 URL。
pub fn try_download(ctx: &Ctx, urls: &[String], dest: &str) -> Result<String, String> {
    if urls.is_empty() {
        return Err("没有可用的下载地址".to_string());
    }
    if ctx.dry_run {
        crate::log::plain(format!(
            "  [dry-run] 下载 {} -> {}(共 {} 个候选源)",
            urls[0],
            dest,
            urls.len()
        ));
        return Ok(urls[0].clone());
    }
    let mut last_err = String::new();
    for url in urls {
        crate::log::plain(format!("  下载 {url}"));
        match download_one(url, dest) {
            Ok(n) => {
                crate::log::ok(format!("已下载 {dest}({:.1} MB)", n as f64 / 1048576.0));
                return Ok(url.clone());
            }
            Err(e) => {
                crate::log::warn(format!("  此源失败: {e}"));
                last_err = e;
            }
        }
    }
    Err(format!("所有下载源均失败,最后错误: {last_err}"))
}

fn download_one(url: &str, dest: &str) -> Result<u64, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(20))
        .timeout(Duration::from_secs(600))
        .build();
    let resp = agent.get(url).call().map_err(|e| format!("{e}"))?;
    let mut reader = resp.into_reader();
    let mut f = File::create(dest).map_err(|e| format!("创建文件失败: {e}"))?;
    let mut buf = [0u8; 65536];
    let mut total = 0u64;
    loop {
        let n = reader.read(&mut buf).map_err(|e| format!("读取失败: {e}"))?;
        if n == 0 {
            break;
        }
        f.write_all(&buf[..n]).map_err(|e| format!("写入失败: {e}"))?;
        total += n as u64;
        if total % (8 * 1024 * 1024) < 65536 {
            print!("\r  已下载 {:.1} MB", total as f64 / 1048576.0);
            use std::io::Write as _;
            let _ = std::io::stdout().flush();
        }
    }
    println!();
    if total == 0 {
        let _ = std::fs::remove_file(dest);
        return Err("下载内容为空".to_string());
    }
    Ok(total)
}
