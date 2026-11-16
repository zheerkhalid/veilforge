//! Sealing of the map, the recovery copies and the journal records.
//!
//! Argon2id(password, salt) gives a 64-byte master key; HKDF-SHA-256 turns it
//! into independent subkeys. A sealed file is a 128-byte header authenticated
//! with HMAC-SHA-256, followed by an XChaCha20-Poly1305 body whose associated
//! data is the header. User files are never encrypted; only these small files.

use crate::model::KdfParams;
use crate::secret::{random_array, LockedBytes};
use argon2::{Algorithm, Argon2, Block, Params, Version};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;
use zeroize::{Zeroize, Zeroizing};

pub const MAGIC_MAP: &[u8; 8] = b"VFMAP\0\0\x01";
pub const MAGIC_RECOVERY: &[u8; 8] = b"VFREC\0\0\x01";
pub const HEADER_LEN: usize = 128;
const MAC_OFFSET: usize = 96;
const TAG_LEN: usize = 16;
const LABEL_MAP: &[u8] = b"VeilForge v1 map encryption";
const LABEL_MAC: &[u8] = b"VeilForge v1 header authentication";
const LABEL_RECOVERY: &[u8] = b"VeilForge v1 recovery copy encryption";
const LABEL_JOURNAL: &[u8] = b"VeilForge v1 journal encryption";
const JOURNAL_AAD: &[u8] = b"VeilForge v1 journal record";
/// Upper bounds accepted from a file (a damaged header must not ask for 1 TiB).
const MAX_M_KIB: u32 = 4 * 1024 * 1024;
const MAX_T: u32 = 64;
const MAX_P: u32 = 64;

#[derive(Debug, PartialEq, Eq)]
pub enum OpenError {
    /// Not a file of the expected kind (magic, length).
    NotOurs,
    /// The header does not authenticate under these keys.
    WrongKey,
    /// The header is fine but the body failed its integrity check.
    Damaged,
}

/// The subkeys of one master password + salt.
pub struct Keys {
    map: LockedBytes,
    mac: LockedBytes,
    recovery: LockedBytes,
    journal: LockedBytes,
    pub salt: [u8; 32],
    pub kdf: KdfParams,
}

fn kdf_acceptable(kdf: &KdfParams) -> bool {
    kdf.t >= 1
        && kdf.p >= 1
        && kdf.t <= MAX_T
        && kdf.p <= MAX_P
        && kdf.m_kib <= MAX_M_KIB
        && kdf.m_kib >= 8 * kdf.p
}

fn subkey(master: &[u8; 64], salt: &[u8; 32], label: &[u8]) -> LockedBytes {
    let hk = Hkdf::<Sha256>::new(Some(salt), master);
    let mut key = LockedBytes::zeroed(32);
    hk.expand(label, key.as_mut_slice())
        .expect("32 bytes is a valid HKDF-SHA-256 output length");
    key
}

impl Keys {
    /// Runs Argon2id. The password copy made here is wiped before returning.
    pub fn derive(password: &str, kdf: &KdfParams, salt: &[u8; 32]) -> Result<Keys, String> {
        if !kdf_acceptable(kdf) {
            return Err("The key-stretching settings are not acceptable.".into());
        }
        let params = Params::new(kdf.m_kib, kdf.t, kdf.p, Some(64)).map_err(|e| e.to_string())?;
        let needed = params.block_count();
        let mut blocks: Vec<Block> = Vec::new();
        blocks
            .try_reserve_exact(needed)
            .map_err(|_| "There is not enough free memory to check the password.".to_string())?;
        blocks.resize(needed, Block::default());
        let pw = Zeroizing::new(password.as_bytes().to_vec());
        let mut master = Zeroizing::new([0u8; 64]);
        let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
        let result = argon.hash_password_into_with_memory(&pw, salt, &mut master[..], &mut blocks);
        for b in blocks.iter_mut() {
            b.zeroize();
        }
        result.map_err(|e| e.to_string())?;
        Ok(Keys {
            map: subkey(&master, salt, LABEL_MAP),
            mac: subkey(&master, salt, LABEL_MAC),
            recovery: subkey(&master, salt, LABEL_RECOVERY),
            journal: subkey(&master, salt, LABEL_JOURNAL),
            salt: *salt,
            kdf: *kdf,
        })
    }

    pub fn fresh(password: &str, kdf: &KdfParams) -> Result<Keys, String> {
        Keys::derive(password, kdf, &random_array::<32>())
    }

    /// Constant-time comparison of two key sets (same password and salt).
    pub fn same(&self, other: &Keys) -> bool {
        let a = self.mac.as_slice().ct_eq(other.mac.as_slice());
        let b = self.map.as_slice().ct_eq(other.map.as_slice());
        bool::from(a & b)
    }

    fn body_key(&self, magic: &[u8; 8]) -> &LockedBytes {
        if magic == MAGIC_RECOVERY {
            &self.recovery
        } else {
            &self.map
        }
    }

    fn header_mac(&self, header: &[u8]) -> [u8; 32] {
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(self.mac.as_slice())
            .expect("HMAC accepts any key length");
        mac.update(header);
        mac.finalize().into_bytes().into()
    }

    /// Seals `plain` into a self-describing file image.
    pub fn seal(&self, magic: &[u8; 8], plain: &[u8]) -> Vec<u8> {
        let nonce = random_array::<24>();
        let mut out = Vec::with_capacity(HEADER_LEN + plain.len() + TAG_LEN);
        out.extend_from_slice(magic);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&[0u8; 2]);
        out.extend_from_slice(&self.kdf.m_kib.to_le_bytes());
        out.extend_from_slice(&self.kdf.t.to_le_bytes());
        out.extend_from_slice(&self.kdf.p.to_le_bytes());
        out.extend_from_slice(&self.salt);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&(plain.len() as u64).to_le_bytes());
        out.resize(MAC_OFFSET, 0);
        let mac = self.header_mac(&out[..MAC_OFFSET]);
        out.extend_from_slice(&mac);
        let cipher = XChaCha20Poly1305::new_from_slice(self.body_key(magic).as_slice())
            .expect("32-byte key");
        let body = cipher
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: plain,
                    aad: &out[..HEADER_LEN],
                },
            )
            .expect("encryption of an in-memory buffer cannot fail");
        out.extend_from_slice(&body);
        out
    }

    /// Opens a file image made by [`Keys::seal`].
    pub fn open(&self, magic: &[u8; 8], file: &[u8]) -> Result<Zeroizing<Vec<u8>>, OpenError> {
        let header = Header::parse(magic, file).ok_or(OpenError::NotOurs)?;
        let mac = self.header_mac(&file[..MAC_OFFSET]);
        if !bool::from(mac.ct_eq(&file[MAC_OFFSET..HEADER_LEN])) {
            return Err(OpenError::WrongKey);
        }
        let body = &file[HEADER_LEN..];
        if body.len() as u64 != header.payload_len + TAG_LEN as u64 {
            return Err(OpenError::Damaged);
        }
        let cipher = XChaCha20Poly1305::new_from_slice(self.body_key(magic).as_slice())
            .expect("32-byte key");
        cipher
            .decrypt(
                XNonce::from_slice(&header.nonce),
                Payload {
                    msg: body,
                    aad: &file[..HEADER_LEN],
                },
            )
            .map(Zeroizing::new)
            .map_err(|_| OpenError::Damaged)
    }

    /// One journal record: nonce + ciphertext.
    pub fn seal_record(&self, plain: &[u8]) -> Vec<u8> {
        let nonce = random_array::<24>();
        let cipher =
            XChaCha20Poly1305::new_from_slice(self.journal.as_slice()).expect("32-byte key");
        let mut out = nonce.to_vec();
        out.extend_from_slice(
            &cipher
                .encrypt(
                    XNonce::from_slice(&nonce),
                    Payload {
                        msg: plain,
                        aad: JOURNAL_AAD,
                    },
                )
                .expect("encryption of an in-memory buffer cannot fail"),
        );
        out
    }

    pub fn open_record(&self, record: &[u8]) -> Option<Zeroizing<Vec<u8>>> {
        if record.len() < 24 + TAG_LEN {
            return None;
        }
        let cipher =
            XChaCha20Poly1305::new_from_slice(self.journal.as_slice()).expect("32-byte key");
        cipher
            .decrypt(
                XNonce::from_slice(&record[..24]),
                Payload {
                    msg: &record[24..],
                    aad: JOURNAL_AAD,
                },
            )
            .ok()
            .map(Zeroizing::new)
    }
}

/// The clear part of a sealed file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub kdf: KdfParams,
    pub salt: [u8; 32],
    pub nonce: [u8; 24],
    pub payload_len: u64,
}

impl Header {
    pub fn parse(magic: &[u8; 8], file: &[u8]) -> Option<Header> {
        if file.len() < HEADER_LEN + TAG_LEN || &file[..8] != magic {
            return None;
        }
        let u32_at = |o: usize| u32::from_le_bytes(file[o..o + 4].try_into().unwrap());
        if u16::from_le_bytes([file[8], file[9]]) != 1 {
            return None;
        }
        let kdf = KdfParams {
            m_kib: u32_at(12),
            t: u32_at(16),
            p: u32_at(20),
        };
        if !kdf_acceptable(&kdf) {
            return None;
        }
        Some(Header {
            kdf,
            salt: file[24..56].try_into().unwrap(),
            nonce: file[56..80].try_into().unwrap(),
            payload_len: u64::from_le_bytes(file[80..88].try_into().unwrap()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_open_round_trip_and_failures() {
        let keys = Keys::fresh("correct horse", &KdfParams::TEST).unwrap();
        let sealed = keys.seal(MAGIC_MAP, b"hello map");
        assert_eq!(&keys.open(MAGIC_MAP, &sealed).unwrap()[..], b"hello map");
        assert!(!sealed.windows(9).any(|w| w == b"hello map"));
        // Wrong magic, wrong password, damaged body, damaged header.
        assert_eq!(
            keys.open(MAGIC_RECOVERY, &sealed).unwrap_err(),
            OpenError::NotOurs
        );
        let other = Keys::derive("wrong", &KdfParams::TEST, &keys.salt).unwrap();
        assert_eq!(
            other.open(MAGIC_MAP, &sealed).unwrap_err(),
            OpenError::WrongKey
        );
        assert!(!keys.same(&other));
        let again = Keys::derive("correct horse", &KdfParams::TEST, &keys.salt).unwrap();
        assert!(keys.same(&again));
        let mut bad = sealed.clone();
        *bad.last_mut().unwrap() ^= 1;
        assert_eq!(keys.open(MAGIC_MAP, &bad).unwrap_err(), OpenError::Damaged);
        let mut bad = sealed.clone();
        bad[60] ^= 1;
        assert_eq!(keys.open(MAGIC_MAP, &bad).unwrap_err(), OpenError::WrongKey);
        // The recovery key is a different key.
        let rec = keys.seal(MAGIC_RECOVERY, b"entry");
        assert_eq!(&keys.open(MAGIC_RECOVERY, &rec).unwrap()[..], b"entry");
        let mut as_map = rec.clone();
        as_map[..8].copy_from_slice(MAGIC_MAP);
        assert!(keys.open(MAGIC_MAP, &as_map).is_err());
    }

    #[test]
    fn journal_records_are_sealed() {
        let keys = Keys::fresh("pw", &KdfParams::TEST).unwrap();
        let rec = keys.seal_record(b"secret name.docx");
        assert!(!rec.windows(6).any(|w| w == b"secret"));
        assert_eq!(&keys.open_record(&rec).unwrap()[..], b"secret name.docx");
        let other = Keys::fresh("pw", &KdfParams::TEST).unwrap();
        assert!(other.open_record(&rec).is_none());
    }

    #[test]
    fn absurd_kdf_costs_are_refused() {
        let huge = KdfParams {
            m_kib: u32::MAX,
            t: 1,
            p: 1,
        };
        assert!(Keys::derive("pw", &huge, &[0u8; 32]).is_err());
    }
}
