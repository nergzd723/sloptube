//! Import a YouTube session from a Chromium-based browser profile.
//!
//! Cookies are AES-128-CBC encrypted with a key derived from either the fixed
//! "peanuts" password (v10) or a password kept in the Secret Service (v11).

use std::path::{Path, PathBuf};

use aes::cipher::{BlockDecryptMut, KeyIvInit, block_padding::Pkcs7};
use anyhow::{Context, Result, bail};
use gtk::glib;

use super::Cookie;

type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;

#[derive(Debug, Clone)]
pub struct BrowserProfile {
    pub browser: &'static str,
    pub profile: String,
    pub cookies_db: PathBuf,
    keyring_label: &'static str,
}

impl BrowserProfile {
    pub fn label(&self) -> String {
        if self.profile == "Default" {
            self.browser.to_owned()
        } else {
            format!("{} — {}", self.browser, self.profile)
        }
    }
}

/// (display name, config dir, Secret Service label)
const BROWSERS: &[(&str, &str, &str)] = &[
    ("Chromium", "chromium", "Chromium Safe Storage"),
    ("Google Chrome", "google-chrome", "Chrome Safe Storage"),
    ("Brave", "BraveSoftware/Brave-Browser", "Brave Safe Storage"),
    ("Vivaldi", "vivaldi", "Chrome Safe Storage"),
    ("Microsoft Edge", "microsoft-edge", "Chromium Safe Storage"),
];

pub fn find_profiles() -> Vec<BrowserProfile> {
    let config = glib::user_config_dir();
    let mut out = Vec::new();
    for (browser, dir, label) in BROWSERS {
        let Ok(entries) = std::fs::read_dir(config.join(dir)) else {
            continue;
        };
        let mut dirs: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_dir())
            .collect();
        dirs.sort();
        for p in dirs {
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if name != "Default" && !name.starts_with("Profile ") {
                continue;
            }
            for rel in ["Network/Cookies", "Cookies"] {
                let db = p.join(rel);
                if db.exists() {
                    out.push(BrowserProfile {
                        browser,
                        profile: name.clone(),
                        cookies_db: db,
                        keyring_label: label,
                    });
                    break;
                }
            }
        }
    }
    out
}

async fn keyring_password(label: &str) -> Option<Vec<u8>> {
    let keyring = oo7::Keyring::new().await.ok()?;
    let _ = keyring.unlock().await;
    for item in keyring.items().await.ok()? {
        if item.label().await.ok().as_deref() == Some(label) {
            return item.secret().await.ok().map(|s| s.as_bytes().to_vec());
        }
    }
    None
}

/// Read and decrypt YouTube/Google cookies (must run on the tokio runtime).
pub async fn import(profile: &BrowserProfile) -> Result<Vec<Cookie>> {
    let password = keyring_password(profile.keyring_label).await;
    let db = profile.cookies_db.clone();
    tokio::task::spawn_blocking(move || read_cookies(&db, password.as_deref())).await?
}

fn derive_key(password: &[u8]) -> [u8; 16] {
    let mut key = [0u8; 16];
    pbkdf2::pbkdf2_hmac::<sha1::Sha1>(password, b"saltysalt", 1, &mut key);
    key
}

fn decrypt(enc: &[u8], v10: &[u8; 16], v11: Option<&[u8; 16]>, strip_hash: bool) -> Option<String> {
    let (key, data) = if let Some(d) = enc.strip_prefix(b"v10") {
        (v10, d)
    } else {
        let d = enc.strip_prefix(b"v11")?;
        (v11?, d)
    };
    let iv = [b' '; 16];
    let mut buf = data.to_vec();
    let plain = Aes128CbcDec::new(key.into(), &iv.into())
        .decrypt_padded_mut::<Pkcs7>(&mut buf)
        .ok()?;
    let plain = if strip_hash && plain.len() >= 32 {
        &plain[32..]
    } else {
        plain
    };
    String::from_utf8(plain.to_vec()).ok()
}

fn read_cookies(db: &Path, v11_password: Option<&[u8]>) -> Result<Vec<Cookie>> {
    // Work on a copy: the browser may hold a lock on the live database.
    let tmp = tempdir()?;
    let copy = tmp.join("Cookies");
    std::fs::copy(db, &copy).context("cannot copy the browser cookie database")?;
    for ext in ["-wal", "-journal"] {
        let side = PathBuf::from(format!("{}{ext}", db.display()));
        if side.exists() {
            let _ = std::fs::copy(&side, PathBuf::from(format!("{}{ext}", copy.display())));
        }
    }
    let result = (|| -> Result<Vec<Cookie>> {
        let conn = rusqlite::Connection::open(&copy)?;
        let meta_version: i64 = conn
            .query_row("SELECT value FROM meta WHERE key='version'", [], |r| {
                r.get::<_, String>(0)
            })
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let strip_hash = meta_version >= 24;
        let v10 = derive_key(b"peanuts");
        let v11 = derive_key(v11_password.unwrap_or(b""));
        let mut stmt = conn.prepare(
            "SELECT host_key, name, value, encrypted_value, path, expires_utc, is_secure, is_httponly \
             FROM cookies WHERE host_key LIKE '%youtube.com' OR host_key LIKE '%google.com'",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Vec<u8>>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, bool>(6)?,
                r.get::<_, bool>(7)?,
            ))
        })?;
        let mut cookies = Vec::new();
        let mut failed = 0;
        for row in rows {
            let (host, name, value, enc, path, expires_utc, secure, http_only) = row?;
            let value = if enc.is_empty() {
                value
            } else {
                match decrypt(&enc, &v10, Some(&v11), strip_hash) {
                    Some(v) => v,
                    None => {
                        failed += 1;
                        continue;
                    }
                }
            };
            let expires = (expires_utc > 0).then(|| expires_utc / 1_000_000 - 11_644_473_600);
            cookies.push(Cookie {
                domain: host,
                name,
                value,
                path,
                expires,
                secure,
                http_only,
            });
        }
        if cookies.is_empty() && failed > 0 {
            bail!("could not decrypt the browser's cookies (is the keyring unlocked?)");
        }
        Ok(cookies)
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

fn tempdir() -> Result<PathBuf> {
    let dir = crate::config::runtime_dir().join(format!("import-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes::cipher::BlockEncryptMut;

    #[test]
    fn v10_roundtrip() {
        type Enc = cbc::Encryptor<aes::Aes128>;
        let key = derive_key(b"peanuts");
        let mut plain = vec![0u8; 32];
        plain.extend_from_slice(b"secret-value");
        let mut buf = vec![0u8; plain.len() + 16];
        buf[..plain.len()].copy_from_slice(&plain);
        let ct = Enc::new((&key).into(), &[b' '; 16].into())
            .encrypt_padded_mut::<Pkcs7>(&mut buf, plain.len())
            .unwrap()
            .to_vec();
        let mut enc = b"v10".to_vec();
        enc.extend(ct);
        assert_eq!(decrypt(&enc, &key, None, true).as_deref(), Some("secret-value"));
    }
}
