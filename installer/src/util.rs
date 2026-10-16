//! 小工具:随机数(优先 /dev/urandom)。

use std::io::Read;

pub fn random_bytes(n: usize) -> Vec<u8> {
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let mut b = vec![0u8; n];
        if f.read_exact(&mut b).is_ok() {
            return b;
        }
    }
    // 兜底:时间 + pid 混合(理论上不会走到)
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9E3779B97F4A7C15);
    let seed = nanos ^ ((std::process::id() as u64) << 32) ^ 0x2545F4914F6CDD1D;
    (0..n)
        .map(|i| {
            let x = seed.wrapping_mul(0x9E3779B97F4A7C15).wrapping_add((i as u64) << 7);
            (x >> ((i % 7) * 8)) as u8
        })
        .collect()
}

pub fn random_alnum(n: usize) -> String {
    const CS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    random_bytes(n)
        .iter()
        .map(|b| CS[(*b as usize) % CS.len()] as char)
        .collect()
}

pub fn random_port() -> u16 {
    let b = random_bytes(2);
    20000 + (u16::from_le_bytes([b[0], b[1]]) % 40000)
}

/// 百分号编码(用于把用户名/密码安全地嵌入 tg:// 等 URL)。
pub fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_alnum_shape() {
        for _ in 0..20 {
            let s = random_alnum(16);
            assert_eq!(s.len(), 16);
            assert!(s.chars().all(|c| c.is_ascii_alphanumeric()));
        }
        assert_ne!(random_alnum(16), random_alnum(16));
    }

    #[test]
    fn random_port_in_range() {
        for _ in 0..50 {
            let p = random_port();
            assert!((20000..60000).contains(&p));
        }
    }

    #[test]
    fn urlencode_cases() {
        assert_eq!(urlencode("abc123"), "abc123");
        assert_eq!(urlencode("a b&c=d"), "a%20b%26c%3Dd");
        assert_eq!(urlencode("密码"), "%E5%AF%86%E7%A0%81");
    }
}
