//! The key this PC uses to prove itself to the tablet. It's the same pair of files adb.exe keeps in
//! `%USERPROFILE%\.android`, so a tablet that already trusts adb.exe trusts this program too, and a
//! PC that has never had adb.exe gets a new pair the first time.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use rsa::pkcs8::{DecodePrivateKey, EncodePrivateKey, LineEnding};
use rsa::traits::PublicKeyParts;
use rsa::{BigUint, RsaPrivateKey};

const KEY_BITS: usize = 2048;
const MODULUS_WORDS: u32 = (KEY_BITS / 32) as u32;

pub struct AdbKey {
    pub private: RsaPrivateKey,
    /// What the tablet is offered when it doesn't know this key yet, as adb.exe would send it.
    pub public_line: String,
}

pub fn key_dir() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("USERPROFILE")?).join(".android"))
}

/// Reads the key, or makes one if there isn't one yet.
pub fn load_or_create(dir: &Path) -> io::Result<AdbKey> {
    let private_path = dir.join("adbkey");
    let public_path = dir.join("adbkey.pub");
    let private = match fs::read_to_string(&private_path) {
        Ok(pem) => RsaPrivateKey::from_pkcs8_pem(&pem).map_err(|e| io::Error::other(format!("can't read {}: {e}", private_path.display())))?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let key = RsaPrivateKey::new(&mut rsa::rand_core::OsRng, KEY_BITS).map_err(io::Error::other)?;
            let pem = key.to_pkcs8_pem(LineEnding::LF).map_err(io::Error::other)?;
            fs::create_dir_all(dir)?;
            fs::write(&private_path, pem.as_bytes())?;
            // A new private key makes any old public key wrong.
            let _ = fs::remove_file(&public_path);
            key
        }
        Err(e) => return Err(e),
    };
    let public_line = match fs::read_to_string(&public_path) {
        Ok(line) if !line.trim().is_empty() => line.trim_end_matches(['\r', '\n', '\0']).to_string(),
        _ => {
            let line = public_key_line(&private, &user_and_host());
            let _ = fs::write(&public_path, &line);
            line
        }
    };
    Ok(AdbKey { private, public_line })
}

/// The name adb.exe puts after the key, which the tablet's prompt shows.
fn user_and_host() -> String {
    let user = std::env::var("USERNAME").unwrap_or_else(|_| "user".to_string());
    let host = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "pc".to_string());
    format!("{user}@{host}")
}

/// Android's form of an RSA public key: the modulus with two values precomputed for Montgomery
/// multiplication, all little-endian.
pub fn android_public_key(key: &RsaPrivateKey) -> Vec<u8> {
    let n = key.n();
    let n_bytes = n.to_bytes_le();
    let n0 = u32::from_le_bytes([n_bytes[0], n_bytes[1], n_bytes[2], n_bytes[3]]);
    // n0's inverse modulo 2^32, by Newton's method: each step doubles the correct bits.
    let mut inverse = n0;
    for _ in 0..5 {
        inverse = inverse.wrapping_mul(2u32.wrapping_sub(n0.wrapping_mul(inverse)));
    }
    let rr = (BigUint::from(1u8) << (KEY_BITS * 2)) % n;
    let words = |value: &BigUint| {
        let mut bytes = value.to_bytes_le();
        bytes.resize(KEY_BITS / 8, 0);
        bytes
    };
    let exponent = u32::from_le_bytes({
        let mut bytes = key.e().to_bytes_le();
        bytes.resize(4, 0);
        [bytes[0], bytes[1], bytes[2], bytes[3]]
    });
    let mut out = Vec::with_capacity(4 + 4 + KEY_BITS / 4 + 4);
    out.extend_from_slice(&MODULUS_WORDS.to_le_bytes());
    out.extend_from_slice(&inverse.wrapping_neg().to_le_bytes());
    out.extend_from_slice(&words(n));
    out.extend_from_slice(&words(&rr));
    out.extend_from_slice(&exponent.to_le_bytes());
    out
}

pub fn public_key_line(key: &RsaPrivateKey, name: &str) -> String {
    format!("{} {name}", base64(&android_public_key(key)))
}

pub fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard_examples() {
        for (input, encoded) in [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foob", "Zm9vYg=="), ("fooba", "Zm9vYmE="), ("foobar", "Zm9vYmFy")] {
            assert_eq!(base64(input.as_bytes()), encoded, "{input}");
        }
    }

    #[test]
    fn a_new_key_is_saved_and_read_back_the_same() {
        let dir = std::env::temp_dir().join(format!("osutab-key-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let made = load_or_create(&dir).unwrap();
        assert!(dir.join("adbkey").exists() && dir.join("adbkey.pub").exists());
        let again = load_or_create(&dir).unwrap();
        assert_eq!(made.public_line, again.public_line);
        assert_eq!(made.private, again.private);
        let encoded = made.public_line.split(' ').next().unwrap();
        assert_eq!(encoded.len(), 524usize.div_ceil(3) * 4);
        let _ = fs::remove_dir_all(&dir);
    }

    /// The real proof: the public key this works out from adb.exe's private key must be exactly the
    /// one adb.exe itself saved. Skipped on a PC that has never run adb.exe.
    #[test]
    fn the_public_key_matches_the_one_adb_made() {
        let Some(dir) = key_dir() else { return };
        let (Ok(pem), Ok(saved)) = (fs::read_to_string(dir.join("adbkey")), fs::read_to_string(dir.join("adbkey.pub"))) else { return };
        let key = RsaPrivateKey::from_pkcs8_pem(&pem).unwrap();
        let ours = base64(&android_public_key(&key));
        assert_eq!(saved.split_whitespace().next().unwrap(), ours);
    }
}
