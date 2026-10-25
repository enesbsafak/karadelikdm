//! HLS AES-128 çözme (RFC 8216 §5.2): AES-128-CBC, PKCS7 dolgu, segment başına IV.

use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};
use anyhow::{anyhow, bail, Result};

type Dec = cbc::Decryptor<aes::Aes128>;

pub(crate) fn decrypt(key: &[u8; 16], iv: &[u8; 16], mut data: Vec<u8>) -> Result<Vec<u8>> {
    let n = Dec::new(&(*key).into(), &(*iv).into())
        .decrypt_padded_mut::<Pkcs7>(&mut data)
        .map_err(|_| anyhow!("segment çözülemedi: anahtar ya da IV yanlış, veya veri eksik"))?
        .len();
    data.truncate(n);
    Ok(data)
}

/// IV verilmemişse segmentin medya sıra numarası 16 baytlık big-endian tamsayı olarak kullanılır.
pub(crate) fn seq_iv(seq: u64) -> [u8; 16] {
    (seq as u128).to_be_bytes()
}

/// `0x` önekli onaltılık IV. Kısa yazılmışsa soldan sıfırla tamamlanır (tamsayı olarak yorumlanır).
pub(crate) fn parse_iv(s: &str) -> Result<[u8; 16]> {
    let h = s.trim().strip_prefix("0x").or_else(|| s.trim().strip_prefix("0X")).unwrap_or(s.trim());
    if h.is_empty() || h.len() > 32 {
        bail!("geçersiz IV: {s}");
    }
    let v = u128::from_str_radix(h, 16).map_err(|_| anyhow!("geçersiz IV: {s}"))?;
    Ok(v.to_be_bytes())
}

pub(crate) fn key_bytes(d: &[u8], url: &str) -> Result<[u8; 16]> {
    d.try_into().map_err(|_| anyhow!("anahtar 16 bayt olmalı, {} bayt geldi: {url}", d.len()))
}

#[cfg(test)]
pub(crate) fn encrypt(key: &[u8; 16], iv: &[u8; 16], plain: &[u8]) -> Vec<u8> {
    use aes::cipher::BlockEncryptMut;
    let mut buf = vec![0u8; plain.len() + 16];
    buf[..plain.len()].copy_from_slice(plain);
    let n = cbc::Encryptor::<aes::Aes128>::new(&(*key).into(), &(*iv).into())
        .encrypt_padded_mut::<Pkcs7>(&mut buf, plain.len())
        .unwrap()
        .len();
    buf.truncate(n);
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_iv() {
        let key = [7u8; 16];
        let iv = seq_iv(0x0102);
        assert_eq!(&iv[14..], &[1, 2]);
        assert!(iv[..14].iter().all(|&b| b == 0));
        let ct = encrypt(&key, &iv, b"merhaba dunya, bu bir segment");
        assert_eq!(ct.len() % 16, 0);
        assert_eq!(decrypt(&key, &iv, ct.clone()).unwrap(), b"merhaba dunya, bu bir segment");
        assert!(decrypt(&[8u8; 16], &iv, ct).is_err());
    }

    #[test]
    fn iv_parsing() {
        assert_eq!(parse_iv("0x00000000000000000000000000000102").unwrap(), seq_iv(0x102));
        assert_eq!(parse_iv("0X102").unwrap(), seq_iv(0x102));
        assert!(parse_iv("0xZZ").is_err());
        assert!(parse_iv("0x").is_err());
    }
}
