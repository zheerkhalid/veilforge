//! Zeroizing containers, locked key pages and the per-session memory cipher.
//!
//! * [`SecretBytes`] / [`SecretString`]: heap buffers wiped when dropped.
//! * [`SecretInput`]: the text buffer behind password fields. It never
//!   reallocates, so wiping it on drop wipes every copy it ever held.
//! * [`LockedBytes`]: key material in its own `VirtualLock`ed page.
//! * [`SessionKey`] / [`Protected`]: while a vault is unlocked, entry secrets
//!   stay encrypted under a random key that exists only for that session and
//!   are decrypted at the moment of use.

use chacha20poly1305::aead::{AeadInPlace, KeyInit};
use chacha20poly1305::{Tag, XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::cell::RefCell;
use std::sync::{Arc, RwLock};
use windows::Win32::System::Memory::{
    VirtualAlloc, VirtualFree, VirtualLock, VirtualUnlock, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE,
    PAGE_READWRITE,
};
use zeroize::{Zeroize, Zeroizing};

pub type SecretBytes = Zeroizing<Vec<u8>>;
pub type SecretString = Zeroizing<String>;

/// Fills `buf` from the operating system's CSPRNG.
pub fn random_bytes(buf: &mut [u8]) {
    getrandom::getrandom(buf).expect("the operating system random generator is unavailable");
}

pub fn random_array<const N: usize>() -> [u8; N] {
    let mut a = [0u8; N];
    random_bytes(&mut a);
    a
}

/// A zeroizing copy of `text` that is allocated once, at its final size.
pub fn secret_string(text: &str) -> SecretString {
    let mut s = String::with_capacity(text.len());
    s.push_str(text);
    Zeroizing::new(s)
}

// --------------------------------------------------------------------------------------
// Text buffer for password fields
// --------------------------------------------------------------------------------------

/// Text typed into a secret field. Fixed capacity: it never reallocates, so
/// there are no stale copies left behind in freed heap memory by this type.
pub struct SecretInput {
    buf: String,
    cap: usize,
}

impl SecretInput {
    /// Bytes of UTF-8 a secret field holds unless created with another size.
    pub const CAPACITY: usize = 4096;

    pub fn new() -> Self {
        Self::with_capacity(Self::CAPACITY)
    }

    /// A buffer for longer text (notes), still fixed in size.
    pub fn with_capacity(cap: usize) -> Self {
        SecretInput {
            buf: String::with_capacity(cap),
            cap,
        }
    }

    pub fn with_text(text: &str) -> Self {
        let mut s = Self::new();
        s.set(text);
        s
    }

    pub fn as_str(&self) -> &str {
        &self.buf
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Replaces the content (cut at the capacity).
    pub fn set(&mut self, text: &str) {
        self.wipe();
        self.push_fitting(text, 0);
    }

    /// Wipes the content but keeps the buffer.
    pub fn wipe(&mut self) {
        // Zeroizes the whole capacity, then sets the length to zero.
        self.buf.zeroize();
    }

    /// A zeroizing copy, for handing to a worker thread.
    pub fn to_secret(&self) -> SecretString {
        secret_string(&self.buf)
    }

    fn push_fitting(&mut self, text: &str, byte_index: usize) -> usize {
        let room = self.cap.saturating_sub(self.buf.len());
        let mut end = text.len().min(room);
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        let part = &text[..end];
        self.buf.insert_str(byte_index, part);
        part.chars().count()
    }
}

impl Default for SecretInput {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for SecretInput {
    fn drop(&mut self) {
        self.buf.zeroize();
    }
}

impl eframe::egui::TextBuffer for SecretInput {
    fn is_mutable(&self) -> bool {
        true
    }

    fn as_str(&self) -> &str {
        &self.buf
    }

    fn insert_text(&mut self, text: &str, char_index: usize) -> usize {
        let byte_index = self.byte_index_from_char_index(char_index);
        self.push_fitting(text, byte_index)
    }

    fn delete_char_range(&mut self, char_range: std::ops::Range<usize>) {
        let start = self.byte_index_from_char_index(char_range.start);
        let end = self.byte_index_from_char_index(char_range.end);
        self.buf.drain(start..end);
    }

    fn clear(&mut self) {
        self.wipe();
    }

    fn replace_with(&mut self, text: &str) {
        self.set(text);
    }
}

// --------------------------------------------------------------------------------------
// A growable buffer that wipes what it leaves behind
// --------------------------------------------------------------------------------------

/// A byte buffer for plaintext that is being assembled. When it has to grow
/// it copies into a new allocation and wipes the old one.
pub struct SecretBuf {
    data: Vec<u8>,
}

impl SecretBuf {
    pub fn with_capacity(cap: usize) -> Self {
        SecretBuf {
            data: Vec::with_capacity(cap.max(64)),
        }
    }

    fn reserve(&mut self, extra: usize) {
        let need = self.data.len() + extra;
        if need <= self.data.capacity() {
            return;
        }
        let mut bigger = Vec::with_capacity(need.max(self.data.capacity() * 2));
        bigger.extend_from_slice(&self.data);
        self.data.zeroize();
        self.data = bigger;
    }

    pub fn extend(&mut self, bytes: &[u8]) {
        self.reserve(bytes.len());
        self.data.extend_from_slice(bytes);
    }

    pub fn resize(&mut self, len: usize) {
        if len > self.data.len() {
            self.reserve(len - self.data.len());
        }
        self.data.resize(len, 0);
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.data
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.data
    }

    /// The underlying vector, for in-place encryption. Callers must not grow it.
    pub fn vec_mut(&mut self) -> &mut Vec<u8> {
        &mut self.data
    }

    /// Hands the bytes over (for buffers that have been encrypted in place).
    pub fn into_vec(mut self) -> Vec<u8> {
        std::mem::take(&mut self.data)
    }
}

impl Drop for SecretBuf {
    fn drop(&mut self) {
        self.data.zeroize();
    }
}

impl std::io::Write for SecretBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.extend(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

// --------------------------------------------------------------------------------------
// Locked key pages
// --------------------------------------------------------------------------------------

/// Key material in a page of its own, locked into RAM (best effort) so it is
/// not written to the page file, and wiped before the page is released.
pub struct LockedBytes {
    ptr: *mut u8,
    len: usize,
    locked: bool,
}

// SAFETY: the page is owned exclusively by this value.
unsafe impl Send for LockedBytes {}
// SAFETY: shared access only ever reads the page.
unsafe impl Sync for LockedBytes {}

const PAGE: usize = 4096;

impl LockedBytes {
    pub fn zeroed(len: usize) -> Self {
        assert!(len > 0 && len <= PAGE);
        // SAFETY: a fresh private read/write page; checked for null below.
        let ptr = unsafe { VirtualAlloc(None, PAGE, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE) }
            as *mut u8;
        assert!(!ptr.is_null(), "could not allocate a key page");
        // SAFETY: the page was just allocated with this size.
        let locked = unsafe { VirtualLock(ptr as *const _, PAGE) }.is_ok();
        LockedBytes { ptr, len, locked }
    }

    pub fn random(len: usize) -> Self {
        let mut k = Self::zeroed(len);
        random_bytes(k.as_mut_slice());
        k
    }

    pub fn from_slice(bytes: &[u8]) -> Self {
        let mut k = Self::zeroed(bytes.len());
        k.as_mut_slice().copy_from_slice(bytes);
        k
    }

    /// Whether `VirtualLock` succeeded for this page.
    pub fn is_locked(&self) -> bool {
        self.locked
    }

    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: ptr is valid for len bytes for the life of self.
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: as above, and self is borrowed mutably.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl Drop for LockedBytes {
    fn drop(&mut self) {
        // SAFETY: the page is still ours; wipe all of it, then release it.
        unsafe {
            std::slice::from_raw_parts_mut(self.ptr, PAGE).zeroize();
            if self.locked {
                let _ = VirtualUnlock(self.ptr as *const _, PAGE);
            }
            let _ = VirtualFree(self.ptr as *mut _, 0, MEM_RELEASE);
        }
    }
}

// --------------------------------------------------------------------------------------
// Session key and protected values
// --------------------------------------------------------------------------------------

/// The random key that protects entry secrets in memory while one vault is
/// unlocked. Wiping it makes every [`Protected`] value of that session
/// permanently unreadable, wherever a copy may still be held.
pub struct SessionKey {
    key: RwLock<Option<LockedBytes>>,
}

impl SessionKey {
    #[allow(clippy::new_ret_no_self)]
    pub fn new() -> Arc<SessionKey> {
        Arc::new(SessionKey {
            key: RwLock::new(Some(LockedBytes::random(32))),
        })
    }

    pub fn wipe(&self) {
        if let Ok(mut k) = self.key.write() {
            *k = None;
        }
    }

    pub fn is_alive(&self) -> bool {
        self.key.read().map(|k| k.is_some()).unwrap_or(false)
    }

    fn cipher(&self) -> Option<XChaCha20Poly1305> {
        let guard = self.key.read().ok()?;
        let key = guard.as_ref()?;
        XChaCha20Poly1305::new_from_slice(key.as_slice()).ok()
    }
}

/// The vault was locked while this value was still held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Locked;

/// A secret (password, TOTP seed, note, hidden field) held encrypted in
/// memory. Decrypt with [`Protected::open`] only at the moment of use.
#[derive(Clone, Default)]
pub struct Protected {
    key: Option<Arc<SessionKey>>,
    nonce: [u8; 24],
    /// Ciphertext followed by the 16-byte tag. Empty for an empty secret.
    ct: Vec<u8>,
}

impl std::fmt::Debug for Protected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Protected(..)")
    }
}

impl Protected {
    pub fn seal(session: &Arc<SessionKey>, plain: &[u8]) -> Protected {
        if plain.is_empty() {
            return Protected::default();
        }
        let Some(cipher) = session.cipher() else {
            return Protected::default();
        };
        let nonce: [u8; 24] = random_array();
        let mut ct = Vec::with_capacity(plain.len() + 16);
        ct.extend_from_slice(plain);
        let tag = cipher
            .encrypt_in_place_detached(XNonce::from_slice(&nonce), b"", &mut ct)
            .expect("in-memory encryption cannot fail for these sizes");
        ct.extend_from_slice(&tag);
        Protected {
            key: Some(session.clone()),
            nonce,
            ct,
        }
    }

    pub fn seal_str(session: &Arc<SessionKey>, plain: &str) -> Protected {
        Self::seal(session, plain.as_bytes())
    }

    pub fn is_empty(&self) -> bool {
        self.ct.is_empty()
    }

    /// Seals other data under the same session key as this value. Empty
    /// when this value has no session (it was empty itself).
    pub fn reseal(&self, plain: &[u8]) -> Protected {
        match &self.key {
            Some(session) => Protected::seal(session, plain),
            None => Protected::default(),
        }
    }

    /// Length of the secret in bytes (known without decrypting).
    pub fn len(&self) -> usize {
        self.ct.len().saturating_sub(16)
    }

    pub fn open(&self) -> Result<SecretBytes, Locked> {
        if self.ct.is_empty() {
            return Ok(Zeroizing::new(Vec::new()));
        }
        let cipher = self.key.as_ref().and_then(|k| k.cipher()).ok_or(Locked)?;
        let (body, tag) = self.ct.split_at(self.ct.len() - 16);
        let mut plain = Zeroizing::new(Vec::with_capacity(body.len()));
        plain.extend_from_slice(body);
        cipher
            .decrypt_in_place_detached(
                XNonce::from_slice(&self.nonce),
                b"",
                &mut plain,
                Tag::from_slice(tag),
            )
            .map_err(|_| Locked)?;
        Ok(plain)
    }

    /// The secret as text. Empty when the vault has been locked.
    pub fn open_str(&self) -> SecretString {
        match self.open() {
            Ok(bytes) => secret_string(std::str::from_utf8(&bytes).unwrap_or("")),
            Err(Locked) => Zeroizing::new(String::new()),
        }
    }

    /// Whether two protected values hold the same secret (decrypts both).
    pub fn same_secret(&self, other: &Protected) -> bool {
        use subtle::ConstantTimeEq;
        match (self.open(), other.open()) {
            (Ok(a), Ok(b)) => a.len() == b.len() && bool::from(a.ct_eq(&b)),
            _ => false,
        }
    }
}

thread_local! {
    static DE_SESSION: RefCell<Option<Arc<SessionKey>>> = const { RefCell::new(None) };
}

/// Runs `f` with `session` as the key that deserialized [`Protected`] values
/// on this thread are sealed under.
pub fn with_session<T>(session: &Arc<SessionKey>, f: impl FnOnce() -> T) -> T {
    let previous = DE_SESSION.with(|s| s.replace(Some(session.clone())));
    let out = f();
    DE_SESSION.with(|s| *s.borrow_mut() = previous);
    out
}

impl Serialize for Protected {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let plain = self
            .open()
            .map_err(|_| serde::ser::Error::custom("the vault is locked"))?;
        serializer.serialize_bytes(&plain)
    }
}

impl<'de> Deserialize<'de> for Protected {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl serde::de::Visitor<'_> for V {
            type Value = Protected;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a byte string")
            }

            fn visit_bytes<E: serde::de::Error>(self, v: &[u8]) -> Result<Protected, E> {
                DE_SESSION.with(|s| match s.borrow().as_ref() {
                    Some(session) => Ok(Protected::seal(session, v)),
                    None => Err(E::custom("no session key on this thread")),
                })
            }

            fn visit_byte_buf<E: serde::de::Error>(self, mut v: Vec<u8>) -> Result<Protected, E> {
                let out = self.visit_bytes(&v);
                v.zeroize();
                out
            }

            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Protected, E> {
                self.visit_bytes(v.as_bytes())
            }
        }
        deserializer.deserialize_byte_buf(V)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::TextBuffer;

    #[test]
    fn protected_round_trip_and_wipe() {
        let s = SessionKey::new();
        let p = Protected::seal_str(&s, "hunter2 ünï");
        assert_eq!(p.open_str().as_str(), "hunter2 ünï");
        assert_eq!(p.len(), "hunter2 ünï".len());
        assert!(!p.ct.windows(7).any(|w| w == b"hunter2"));
        let copy = p.clone();
        s.wipe();
        assert!(!s.is_alive());
        assert!(copy.open().is_err());
        assert_eq!(copy.open_str().as_str(), "");
    }

    #[test]
    fn protected_is_bound_to_its_session() {
        let a = SessionKey::new();
        let b = SessionKey::new();
        let mut p = Protected::seal_str(&a, "secret");
        p.key = Some(b);
        assert!(p.open().is_err());
        assert!(Protected::default().is_empty());
        assert_eq!(Protected::default().open().unwrap().len(), 0);
    }

    #[test]
    fn secret_input_never_reallocates() {
        let mut s = SecretInput::new();
        let ptr = s.buf.as_ptr();
        let cap = s.buf.capacity();
        for _ in 0..1000 {
            s.insert_text("abcdefgh€", 0);
        }
        assert_eq!(s.buf.capacity(), cap);
        assert_eq!(s.buf.as_ptr(), ptr);
        assert!(s.buf.len() <= SecretInput::CAPACITY);
        s.delete_char_range(0..3);
        s.set("x");
        assert_eq!(s.as_str(), "x");
        assert_eq!(s.buf.as_ptr(), ptr);
        s.wipe();
        assert!(s.is_empty());
        // SAFETY: reading the still-owned capacity of the buffer.
        let raw = unsafe { std::slice::from_raw_parts(ptr, cap) };
        assert!(raw.iter().all(|b| *b == 0));
    }

    #[test]
    fn locked_bytes_hold_data() {
        let k = LockedBytes::from_slice(&[7u8; 32]);
        assert_eq!(k.as_slice(), &[7u8; 32]);
        let r = LockedBytes::random(32);
        assert_ne!(r.as_slice(), &[0u8; 32]);
    }

    #[test]
    fn secret_buf_grows() {
        use std::io::Write;
        let mut b = SecretBuf::with_capacity(4);
        for i in 0..1000u32 {
            b.write_all(&i.to_le_bytes()).unwrap();
        }
        assert_eq!(b.len(), 4000);
        assert_eq!(&b.as_slice()[4..8], &1u32.to_le_bytes());
    }
}
