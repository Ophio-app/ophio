// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::constants::{IDENTITY_ENTROPY, IDENTITY_LIMIT};
use crate::dpapi::envelope::{self, SEALED_LIMIT};
use std::path::PathBuf;
use windows::Win32::Security::Cryptography::*;
use zeroize::{Zeroize, Zeroizing};

struct Blob(CRYPT_INTEGER_BLOB);
impl Blob {
    fn bytes(&self) -> io::Result<&[u8]> {
        if self.0.cbData == 0 {
            return Ok(&[]);
        }
        if self.0.pbData.is_null() {
            return Err(denied());
        }
        // SAFETY: this wrapper owns a successful DPAPI allocation of cbData bytes.
        Ok(unsafe { std::slice::from_raw_parts(self.0.pbData, self.0.cbData as usize) })
    }
}
impl Drop for Blob {
    fn drop(&mut self) {
        if !self.0.pbData.is_null() {
            // SAFETY: DPAPI allocated cbData bytes with LocalAlloc. Wipe before
            // freeing, including on errors after successful decryption.
            unsafe {
                std::slice::from_raw_parts_mut(self.0.pbData, self.0.cbData as usize).zeroize();
                let _ = LocalFree(Some(HLOCAL(self.0.pbData.cast())));
            }
        }
    }
}
fn blob(bytes: &[u8]) -> CRYPT_INTEGER_BLOB {
    CRYPT_INTEGER_BLOB {
        cbData: bytes.len() as u32,
        pbData: bytes.as_ptr().cast_mut(),
    }
}
pub fn protect(bytes: &[u8]) -> io::Result<Vec<u8>> {
    if bytes.len() > IDENTITY_LIMIT {
        return Err(denied());
    }
    let input = blob(bytes);
    let entropy = blob(IDENTITY_ENTROPY);
    let mut output = Blob(CRYPT_INTEGER_BLOB::default());
    // SAFETY: inputs remain live; DPAPI treats input buffers as read-only. Only
    // UI_FORBIDDEN is set, so this is current-user scope, never local-machine.
    unsafe {
        CryptProtectData(
            &input,
            PCWSTR::null(),
            Some(&entropy),
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output.0,
        )
        .map_err(failure)?;
    }
    envelope::encode(output.bytes()?)
}
pub fn unprotect(bytes: &[u8]) -> io::Result<Zeroizing<Vec<u8>>> {
    let bytes = envelope::payload(bytes)?;
    let input = blob(bytes);
    let entropy = blob(IDENTITY_ENTROPY);
    let mut output = Blob(CRYPT_INTEGER_BLOB::default());
    // SAFETY: live bounded inputs and initialized output; UI is prohibited.
    unsafe {
        CryptUnprotectData(
            &input,
            None,
            Some(&entropy),
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output.0,
        )
        .map_err(failure)?;
    }
    if output.0.cbData as usize > IDENTITY_LIMIT {
        return Err(denied());
    }
    Ok(Zeroizing::new(output.bytes()?.to_vec()))
}
/// Identity storage adapter. Bind the companion's SecretStore trait to these
/// load/save methods; file bytes are always a DPAPI envelope.
#[derive(Debug)]
pub struct SecretStore {
    path: PathBuf,
}
impl SecretStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
    pub fn load(&self) -> io::Result<Option<Zeroizing<Vec<u8>>>> {
        match super::fs::protected_read(&self.path, SEALED_LIMIT) {
            Ok(bytes) => unprotect(&bytes).map(Some),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }
    pub fn save(&self, bytes: &[u8], replace: bool) -> io::Result<()> {
        super::fs::atomic_write(&self.path, &protect(bytes)?, replace)
    }
}
