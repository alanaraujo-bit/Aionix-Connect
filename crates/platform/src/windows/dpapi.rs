//! Secret protection with the Windows Data Protection API.
//!
//! * [`DataScope::User`]: only the same Windows user can decrypt (Technician, Quick
//!   Support).
//! * [`DataScope::Machine`]: any process on this machine can decrypt, so the file must
//!   additionally be ACL-restricted to SYSTEM/Administrators (Agent, see installer).
//!
//! An entropy value bound to the product prevents other DPAPI consumers from
//! decrypting our blobs by accident.

use windows::Win32::{
    Foundation::{HLOCAL, LocalFree},
    Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_LOCAL_MACHINE, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData,
        CryptUnprotectData,
    },
};

use crate::{DataScope, PlatformError};

const ENTROPY: &[u8] = b"aionix-connect/identity/v1";

fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
    CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 }
}

fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
    let v = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize) }.to_vec();
    unsafe {
        let _ = LocalFree(Some(HLOCAL(out.pbData.cast())));
    }
    v
}

pub fn protect(data: &[u8], scope: DataScope) -> Result<Vec<u8>, PlatformError> {
    let input = blob(data);
    let entropy = blob(ENTROPY);
    let mut out = CRYPT_INTEGER_BLOB::default();
    let mut flags = CRYPTPROTECT_UI_FORBIDDEN;
    if scope == DataScope::Machine {
        flags |= CRYPTPROTECT_LOCAL_MACHINE;
    }
    unsafe { CryptProtectData(&input, None, Some(&entropy), None, None, flags, &mut out)? };
    Ok(take(out))
}

pub fn unprotect(data: &[u8]) -> Result<Vec<u8>, PlatformError> {
    let input = blob(data);
    let entropy = blob(ENTROPY);
    let mut out = CRYPT_INTEGER_BLOB::default();
    unsafe { CryptUnprotectData(&input, None, Some(&entropy), None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out)? };
    Ok(take(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_user_scope() {
        let secret = b"0123456789abcdef0123456789abcdef";
        let sealed = protect(secret, DataScope::User).unwrap();
        assert_ne!(&sealed[..], &secret[..]);
        assert_eq!(unprotect(&sealed).unwrap(), secret);
    }

    #[test]
    fn tampered_blob_is_rejected() {
        let mut sealed = protect(b"secret", DataScope::User).unwrap();
        let last = sealed.len() - 1;
        sealed[last] ^= 0xFF;
        assert!(unprotect(&sealed).is_err());
    }
}
