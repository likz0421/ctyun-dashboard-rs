use aes_gcm::aead::{Aead, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Nonce};
use md5::Md5;
use num_bigint::BigUint;
use rand::RngCore;
use sha1::Sha1;
use sha2::{Digest, Sha256};

pub fn sha256_hex(data: impl AsRef<[u8]>) -> String {
    hex::encode(Sha256::digest(data.as_ref()))
}

pub fn sha256_bytes(data: impl AsRef<[u8]>) -> Vec<u8> {
    Sha256::digest(data.as_ref()).to_vec()
}

pub fn md5_hex(data: impl AsRef<[u8]>) -> String {
    let mut hasher = Md5::new();
    hasher.update(data.as_ref());
    hex::encode(hasher.finalize())
}

pub fn password_hash(password: &str) -> String {
    sha256_hex(format!("{}ctyun_salt_2026", password))
}

/// MGF1 mask generation function using SHA-1 (matches upstream implementation).
fn mgf1(seed: &[u8], mask_len: usize) -> Vec<u8> {
    let mut mask = Vec::with_capacity(mask_len + 20);
    let mut counter: u32 = 0;
    while mask.len() < mask_len {
        let mut input = Vec::with_capacity(seed.len() + 4);
        input.extend_from_slice(seed);
        input.extend_from_slice(&counter.to_be_bytes());
        let digest = Sha1::digest(&input);
        mask.extend_from_slice(&digest);
        counter = counter.wrapping_add(1);
    }
    mask.truncate(mask_len);
    mask
}

/// OAEP encryption with SHA-1. NOTE: delimiter byte position intentionally
/// mirrors the upstream JS implementation (db[len - 1 - label.len() - 1]).
pub fn oaep_encrypt(key_len: usize, label: &[u8], n: &BigUint, e: &BigUint) -> Option<Vec<u8>> {
    let h_len = 20usize;
    if key_len <= h_len + 1 {
        return None;
    }
    let db_len = key_len - h_len - 1;
    let mut db = vec![0u8; db_len];
    let l_hash = Sha1::digest(label);
    db[..h_len.min(l_hash.len())].copy_from_slice(&l_hash[..h_len.min(l_hash.len())]);

    let delimiter_pos = db.len().checked_sub(1 + label.len() + 1)?;
    if delimiter_pos < h_len {
        return None;
    }
    db[delimiter_pos] = 1;

    let mut seed = vec![0u8; h_len];
    OsRng.fill_bytes(&mut seed);

    let db_mask = mgf1(&seed, db_len);
    for (b, m) in db.iter_mut().zip(db_mask.iter()) {
        *b ^= *m;
    }
    let seed_mask = mgf1(&db, h_len);
    for (b, m) in seed.iter_mut().zip(seed_mask.iter()) {
        *b ^= *m;
    }

    let mut em = vec![0u8; key_len];
    em[1..=h_len].copy_from_slice(&seed);
    em[h_len + 1..].copy_from_slice(&db);

    let m = BigUint::from_bytes_be(&em);
    let c = m.modpow(e, n);
    let raw = c.to_bytes_be();
    if raw.len() > key_len {
        return None;
    }
    let mut out = vec![0u8; key_len - raw.len()];
    out.extend_from_slice(&raw);
    Some(out)
}

/// Build the 132-byte response to a REDQ challenge frame.
pub fn redq_response(data: &[u8]) -> Option<Vec<u8>> {
    // inbound = &data[16..]；n_buf 取 inbound[32..161]，e_buf 取 inbound[163..166]
    // 因此至少需要 16 + 166 = 182 字节，否则切片越界 panic。
    if data.len() < 182 {
        return None;
    }
    let inbound = &data[16..];
    let n_buf = &inbound[32..32 + 129];
    let e_buf = &inbound[163..163 + 3];
    let e = ((e_buf[0] as u64) << 16) | ((e_buf[1] as u64) << 8) | (e_buf[2] as u64);
    let n = BigUint::from_bytes_be(n_buf);
    let encrypted = oaep_encrypt(128, b"", &n, &BigUint::from(e))?;

    let mut result = Vec::with_capacity(4 + encrypted.len());
    result.extend_from_slice(&1u32.to_le_bytes());
    result.extend_from_slice(&encrypted);
    Some(result)
}

/// AES-256-GCM encryption for credentials at rest: ENC:iv:tag:cipher (hex).
pub fn encrypt_secret(key: &[u8], plaintext: &str) -> Result<String, String> {
    if key.len() != 32 {
        return Err("master key must be 32 bytes".into());
    }
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|e| e.to_string())?;
    let mut iv = [0u8; 12];
    OsRng.fill_bytes(&mut iv);
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&iv), plaintext.as_bytes())
        .map_err(|e| e.to_string())?;
    // GCM tag is the last 16 bytes returned by aes-gcm.
    let (body, tag) = ciphertext.split_at(ciphertext.len() - 16);
    Ok(format!(
        "ENC:{}:{}:{}",
        hex::encode(iv),
        hex::encode(tag),
        hex::encode(body)
    ))
}

pub fn decrypt_secret(key: &[u8], value: &str) -> Result<String, String> {
    if !value.starts_with("ENC:") {
        return Ok(value.to_string());
    }
    let parts: Vec<&str> = value.split(':').collect();
    if parts.len() != 4 {
        return Err("malformed encrypted secret".into());
    }
    let iv = hex::decode(parts[1]).map_err(|e| e.to_string())?;
    let tag = hex::decode(parts[2]).map_err(|e| e.to_string())?;
    let body = hex::decode(parts[3]).map_err(|e| e.to_string())?;
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|e| e.to_string())?;
    let mut combined = body;
    combined.extend_from_slice(&tag);
    let plain = cipher
        .decrypt(Nonce::from_slice(&iv), combined.as_slice())
        .map_err(|e| e.to_string())?;
    String::from_utf8(plain).map_err(|e| e.to_string())
}

/// AES-128-ECB decryption (used by the AI chat SSO flow).
pub fn aes_ecb_decrypt(key: &[u8], data: &[u8]) -> Result<Vec<u8>, String> {
    use aes::cipher::{BlockDecrypt, KeyInit as _};
    let cipher = aes::Aes128::new_from_slice(key).map_err(|e| e.to_string())?;
    let mut out = data.to_vec();
    if out.len() % 16 != 0 {
        return Err("ciphertext length must be a multiple of 16".into());
    }
    for chunk in out.chunks_exact_mut(16) {
        let mut block = aes::cipher::Block::<aes::Aes128>::default();
        block.copy_from_slice(chunk);
        cipher.decrypt_block(&mut block);
        chunk.copy_from_slice(&block);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redq_response_length() {
        let mut frame = vec![0u8; 200];
        frame[0..4].copy_from_slice(b"REDQ");
        frame[16 + 32] = 0x01; // N high byte (non-zero)
        frame[16 + 163] = 0x01;
        frame[16 + 165] = 0x01; // e = 65537
        let r = redq_response(&frame).expect("response");
        assert_eq!(r.len(), 132);
        assert_eq!(&r[0..4], &1u32.to_le_bytes());
    }

    #[test]
    fn secret_roundtrip() {
        let key = [7u8; 32];
        let enc = encrypt_secret(&key, "hello").unwrap();
        assert_eq!(decrypt_secret(&key, &enc).unwrap(), "hello");
    }
}
