//! Password hashing (Argon2id, PHC strings).
//!
//! Hashing runs on the blocking pool. Unknown accounts are verified against a fixed
//! dummy hash so response time does not reveal whether an e-mail is registered.

use std::sync::OnceLock;

use argon2::{
    Argon2,
    password_hash::{PasswordHasher, PasswordVerifier, phc::PasswordHash},
};

pub const MIN_PASSWORD_LEN: usize = 12;
pub const MAX_PASSWORD_LEN: usize = 256;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PasswordPolicyError {
    #[error("password must have at least {MIN_PASSWORD_LEN} characters")]
    TooShort,
    #[error("password must have at most {MAX_PASSWORD_LEN} characters")]
    TooLong,
}

/// Temporary password handed to a new or reset account: 16 symbols from an alphabet
/// without look-alikes (0/O, 1/l/I), shown as four groups (`xxxx-xxxx-xxxx-xxxx`,
/// about 82 bits). Generated from the operating system's CSPRNG.
pub fn temporary() -> String {
    const ALPHABET: &[u8] = b"abcdefghijkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    // Rejection sampling keeps every symbol equally likely.
    let limit = 256 - (256 % ALPHABET.len());
    let mut symbols = Vec::with_capacity(16);
    while symbols.len() < 16 {
        for &b in aionix_crypto::random_bytes(32).iter() {
            if (b as usize) < limit && symbols.len() < 16 {
                symbols.push(ALPHABET[b as usize % ALPHABET.len()] as char);
            }
        }
    }
    symbols
        .chunks(4)
        .map(|c| c.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("-")
}

pub fn check_policy(password: &str) -> Result<(), PasswordPolicyError> {
    let n = password.chars().count();
    if n < MIN_PASSWORD_LEN {
        Err(PasswordPolicyError::TooShort)
    } else if password.len() > MAX_PASSWORD_LEN {
        Err(PasswordPolicyError::TooLong)
    } else {
        Ok(())
    }
}

fn hash_sync(password: &[u8]) -> String {
    let hash: PasswordHash = Argon2::default()
        .hash_password(password)
        .expect("argon2 hashing with default parameters cannot fail");
    hash.to_string()
}

fn verify_sync(password: &[u8], phc: &str) -> bool {
    match PasswordHash::new(phc) {
        Ok(parsed) => Argon2::default().verify_password(password, &parsed).is_ok(),
        Err(_) => false,
    }
}

fn dummy_hash() -> &'static str {
    static DUMMY: OnceLock<String> = OnceLock::new();
    DUMMY.get_or_init(|| hash_sync(b"aionix-dummy-password-for-timing"))
}

pub async fn hash(password: String) -> String {
    tokio::task::spawn_blocking(move || hash_sync(password.as_bytes()))
        .await
        .expect("hash task")
}

/// Verifies `password` against `phc`, or against the dummy hash when there is no
/// account (always false then), taking comparable time either way.
pub async fn verify(password: String, phc: Option<String>) -> bool {
    tokio::task::spawn_blocking(move || match phc {
        Some(h) => verify_sync(password.as_bytes(), &h),
        None => {
            let _ = verify_sync(password.as_bytes(), dummy_hash());
            false
        }
    })
    .await
    .expect("verify task")
}

#[cfg(test)]
mod tests {
    #[test]
    fn temporary_passwords_are_readable_and_pass_the_policy() {
        let a = super::temporary();
        let b = super::temporary();
        assert_ne!(a, b);
        assert_eq!(a.len(), 19);
        assert_eq!(a.matches('-').count(), 3);
        assert!(!a.contains(['0', 'O', '1', 'l', 'I']));
        assert!(super::check_policy(&a).is_ok());
    }

    use super::*;

    #[tokio::test]
    async fn hashes_verify_and_use_argon2id() {
        let h = hash("correct horse battery".into()).await;
        assert!(h.starts_with("$argon2id$"));
        assert!(!h.contains("correct horse"));
        assert!(verify("correct horse battery".into(), Some(h.clone())).await);
        assert!(!verify("wrong horse battery".into(), Some(h)).await);
        assert!(!verify("anything".into(), None).await);
        assert!(!verify("x".into(), Some("not-a-phc".into())).await);
    }

    #[test]
    fn policy() {
        assert_eq!(check_policy("short"), Err(PasswordPolicyError::TooShort));
        assert!(check_policy("twelve chars").is_ok());
        assert_eq!(
            check_policy(&"x".repeat(300)),
            Err(PasswordPolicyError::TooLong)
        );
    }
}
