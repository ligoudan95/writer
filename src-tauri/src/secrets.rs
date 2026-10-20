//! Secrets 服务（5.1 契约 / NFR-08）：WebDAV 密码、AI Key 经 DPAPI 加密存取，
//! 前端永不接触明文。密文落盘于应用配置目录（%APPDATA%\Writer\secrets.json），
//! 绝不写入库文件、绝不上传（FR-SYN-01）。

use std::{fs, path::PathBuf};

use windows::Win32::Foundation::{LocalFree, HLOCAL};
use windows::Win32::Security::Cryptography::{
    CryptProtectData, CryptUnprotectData, CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN,
};

use crate::library::AppError;

fn store_path() -> Result<PathBuf, AppError> {
    let dir = dirs::config_dir()
        .ok_or_else(|| AppError::new("io", "无法定位配置目录"))?
        .join("Writer");
    fs::create_dir_all(&dir)?;
    Ok(dir.join("secrets.json"))
}

fn load_store() -> std::collections::BTreeMap<String, String> {
    store_path()
        .ok()
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_store(map: &std::collections::BTreeMap<String, String>) -> Result<(), AppError> {
    let p = store_path()?;
    let json = serde_json::to_string_pretty(map)
        .map_err(|e| AppError::new("io", e.to_string()))?;
    crate::library::atomic_write(&p, json.as_bytes())?;
    Ok(())
}

fn dpapi_protect(plain: &[u8]) -> Result<Vec<u8>, AppError> {
    unsafe {
        let mut in_blob = CRYPT_INTEGER_BLOB {
            cbData: plain.len() as u32,
            pbData: plain.as_ptr() as *mut u8,
        };
        let mut out = CRYPT_INTEGER_BLOB::default();
        CryptProtectData(
            &mut in_blob,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out,
        )
        .map_err(|e| AppError::new("secrets", format!("DPAPI 加密失败：{e}")))?;
        let bytes = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(out.pbData as *mut core::ffi::c_void)));
        Ok(bytes)
    }
}

fn dpapi_unprotect(cipher: &[u8]) -> Result<Vec<u8>, AppError> {
    unsafe {
        let mut in_blob = CRYPT_INTEGER_BLOB {
            cbData: cipher.len() as u32,
            pbData: cipher.as_ptr() as *mut u8,
        };
        let mut out = CRYPT_INTEGER_BLOB::default();
        CryptUnprotectData(
            &mut in_blob,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out,
        )
        .map_err(|e| AppError::new("secrets", format!("DPAPI 解密失败：{e}")))?;
        let bytes = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(out.pbData as *mut core::ffi::c_void)));
        Ok(bytes)
    }
}

/// 写入凭据（明文只在本函数内存中出现，落盘前已 DPAPI 加密）
pub fn set(key: &str, value: &str) -> Result<(), AppError> {
    let mut map = load_store();
    let cipher = dpapi_protect(value.as_bytes())?;
    map.insert(key.to_string(), base64_encode(&cipher));
    save_store(&map)
}

/// 读取凭据；不存在返回 None
pub fn get(key: &str) -> Result<Option<String>, AppError> {
    let map = load_store();
    match map.get(key) {
        Some(b64) => {
            let cipher = base64_decode(b64)
                .ok_or_else(|| AppError::new("secrets", "凭据密文损坏"))?;
            Ok(Some(
                String::from_utf8(dpapi_unprotect(&cipher)?)
                    .map_err(|_| AppError::new("secrets", "凭据不是合法 UTF-8"))?,
            ))
        }
        None => Ok(None),
    }
}

// 5.1 Secrets 契约方法：凭据轮换/解除绑定时使用
#[allow(dead_code)]
pub fn delete(key: &str) -> Result<(), AppError> {
    let mut map = load_store();
    map.remove(key);
    save_store(&map)
}

// -- 极简 base64（避免为一个功能引依赖） ------------------------------------

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { B64[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { B64[n as usize & 63] as char } else { '=' });
    }
    out
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a' + 26) as u32),
            b'0'..=b'9' => Some((c - b'0' + 52) as u32),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let s: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    for chunk in s.chunks(4) {
        if chunk.len() < 2 || chunk[0] == b'=' {
            break;
        }
        let v0 = val(chunk[0])?;
        let v1 = val(chunk[1])?;
        let v2 = if chunk.len() > 2 && chunk[2] != b'=' { val(chunk[2])? } else { 0 };
        let v3 = if chunk.len() > 3 && chunk[3] != b'=' { val(chunk[3])? } else { 0 };
        let n = (v0 << 18) | (v1 << 12) | (v2 << 6) | v3;
        out.push((n >> 16) as u8);
        if chunk.len() > 2 && chunk[2] != b'=' {
            out.push((n >> 8) as u8);
        }
        if chunk.len() > 3 && chunk[3] != b'=' {
            out.push(n as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_roundtrip() {
        for case in ["", "f", "fo", "foo", "foob", "fooba", "foobar", "\u{0}\u{ff}\u{10}"] {
            assert_eq!(base64_decode(&base64_encode(case.as_bytes())).unwrap(), case.as_bytes());
        }
    }

    #[test]
    #[ignore = "DPAPI 绑定当前用户，仅在交互环境验证"]
    fn dpapi_roundtrip() {
        set("test-key", "秘密密码123").unwrap();
        assert_eq!(get("test-key").unwrap().as_deref(), Some("秘密密码123"));
        delete("test-key").unwrap();
        assert_eq!(get("test-key").unwrap(), None);
    }
}
