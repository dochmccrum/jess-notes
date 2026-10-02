//! Auth (DESIGN §14.1): single account, argon2id password, per-device bearer tokens (only their
//! SHA-256 is stored), one-time pairing codes, setup code (D10), and login rate limiting.

use crate::error::Result;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use jess_core::Id;
use rand::{Rng, RngCore};
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::net::IpAddr;

pub const PAIRING_TTL_MS: u64 = 10 * 60_000;

fn argon() -> Argon2<'static> {
    Argon2::new(
        Algorithm::Argon2id,
        Version::V0x13,
        Params::new(64 * 1024, 3, 1, None).expect("params"),
    )
}

/// For a high-entropy random secret rather than a password (a local space's, DESIGN §24.1):
/// a slow KDF protects guessable passwords, and 192 random bits aren't guessable, so the
/// cheapest Argon2id parameters do. Verification reads the parameters from the stored hash.
pub fn hash_random_secret(secret: &str) -> String {
    let salt = SaltString::generate(&mut argon2::password_hash::rand_core::OsRng);
    Argon2::new(
        Algorithm::Argon2id,
        Version::V0x13,
        Params::new(Params::MIN_M_COST.max(8), 1, 1, None).expect("params"),
    )
    .hash_password(secret.as_bytes(), &salt)
    .expect("argon2")
    .to_string()
}

pub fn hash_password(pw: &str) -> String {
    let salt = SaltString::generate(&mut argon2::password_hash::rand_core::OsRng);
    argon()
        .hash_password(pw.as_bytes(), &salt)
        .expect("argon2")
        .to_string()
}

pub fn verify_password(pw: &str, phc: &str) -> bool {
    match PasswordHash::new(phc) {
        Ok(h) => argon().verify_password(pw.as_bytes(), &h).is_ok(),
        Err(_) => false,
    }
}

pub fn token_hash(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

pub fn random_token() -> String {
    let mut b = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut b);
    hex::encode(b)
}

/// `XXXX-XXXX` from an unambiguous alphabet.
pub fn setup_code() -> String {
    const A: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut r = rand::thread_rng();
    let s: String = (0..8).map(|_| A[r.gen_range(0..A.len())] as char).collect();
    format!("{}-{}", &s[..4], &s[4..])
}

pub fn account_hash(conn: &Connection) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT password_hash FROM account WHERE id = 1", [], |r| {
            r.get(0)
        })
        .optional()?)
}

pub fn set_password(conn: &Connection, phc: &str, from_env: bool, now: u64) -> Result<()> {
    conn.execute(
        "INSERT INTO account(id, password_hash, created_at, from_env) VALUES (1, ?1, ?2, ?3)
         ON CONFLICT(id) DO UPDATE SET password_hash = excluded.password_hash, from_env = excluded.from_env",
        params![phc, now as i64, from_env as i64],
    )?;
    Ok(())
}

pub fn create_device(conn: &Connection, name: &str, token: &str, now: u64) -> Result<Id> {
    let mut b = [0u8; 10];
    rand::thread_rng().fill_bytes(&mut b);
    let id = Id::new_v7(now, b);
    conn.execute(
        "INSERT INTO devices(id, name, token_hash, created_at, last_seen) VALUES (?1, ?2, ?3, ?4, ?4)",
        params![id.0.to_vec(), name.chars().take(80).collect::<String>(), token_hash(token).to_vec(), now as i64],
    )?;
    Ok(id)
}

pub fn load_tokens(conn: &Connection) -> Result<HashMap<[u8; 32], Id>> {
    let mut st = conn.prepare("SELECT token_hash, id FROM devices WHERE revoked_at IS NULL")?;
    let v = st
        .query_map([], |r| {
            Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, Vec<u8>>(1)?))
        })?
        .filter_map(|r| r.ok())
        .filter_map(|(t, i)| {
            Some((
                <[u8; 32]>::try_from(t.as_slice()).ok()?,
                Id::from_slice(&i)?,
            ))
        })
        .collect();
    Ok(v)
}

#[derive(serde::Serialize, Clone, Debug)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub created_at: i64,
    pub last_seen: Option<i64>,
    pub revoked_at: Option<i64>,
}

pub fn list_devices(conn: &Connection) -> Result<Vec<DeviceInfo>> {
    let mut st = conn.prepare(
        "SELECT id, name, created_at, last_seen, revoked_at FROM devices ORDER BY created_at",
    )?;
    let v = st
        .query_map([], |r| {
            let id: Vec<u8> = r.get(0)?;
            Ok(DeviceInfo {
                id: Id::from_slice(&id)
                    .map(|i| i.to_string())
                    .unwrap_or_default(),
                name: r.get(1)?,
                created_at: r.get(2)?,
                last_seen: r.get(3)?,
                revoked_at: r.get(4)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(v)
}

pub fn revoke_device(conn: &Connection, id: Id, now: u64) -> Result<bool> {
    Ok(conn.execute(
        "UPDATE devices SET revoked_at = ?1 WHERE id = ?2 AND revoked_at IS NULL",
        params![now as i64, id.0.to_vec()],
    )? > 0)
}

pub fn touch_device(conn: &Connection, id: Id, now: u64) -> Result<()> {
    conn.execute(
        "UPDATE devices SET last_seen = ?1 WHERE id = ?2",
        params![now as i64, id.0.to_vec()],
    )?;
    Ok(())
}

/// Creates a single-use pairing code (128-bit), returning it.
pub fn create_pairing(conn: &Connection, now: u64) -> Result<String> {
    let mut b = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut b);
    let code = hex::encode(b);
    conn.execute(
        "DELETE FROM pairing_codes WHERE expires_at < ?1",
        [now as i64],
    )?;
    conn.execute(
        "INSERT INTO pairing_codes(code_hash, expires_at) VALUES (?1, ?2)",
        params![token_hash(&code).to_vec(), (now + PAIRING_TTL_MS) as i64],
    )?;
    Ok(code)
}

/// Consumes a pairing code. Returns true if it was valid and unused.
pub fn redeem_pairing(conn: &Connection, code: &str, now: u64) -> Result<bool> {
    Ok(conn.execute(
        "UPDATE pairing_codes SET used_at = ?1 WHERE code_hash = ?2 AND used_at IS NULL AND expires_at >= ?1",
        params![now as i64, token_hash(code).to_vec()],
    )? == 1)
}

/// Login throttling: 5 attempts/minute/IP, plus a global exponential lockout after 20 failures
/// in an hour. Checked *before* the argon2 verification.
pub struct RateLimiter {
    per_ip: HashMap<IpAddr, Vec<u64>>,
    failures: Vec<u64>,
    per_minute: usize,
}

impl Default for RateLimiter {
    fn default() -> Self {
        RateLimiter::new(5)
    }
}

impl RateLimiter {
    pub fn new(per_minute: usize) -> RateLimiter {
        RateLimiter {
            per_ip: HashMap::new(),
            failures: Vec::new(),
            per_minute: per_minute.max(1),
        }
    }
    /// `Err(retry_after_ms)` if the attempt must be refused.
    pub fn check(&mut self, ip: IpAddr, now: u64) -> std::result::Result<(), u64> {
        let v = self.per_ip.entry(ip).or_default();
        v.retain(|t| now < t + 60_000);
        if v.len() >= self.per_minute {
            return Err(v[0] + 60_000 - now);
        }
        self.failures.retain(|t| now < t + 3_600_000);
        if self.failures.len() >= 20 {
            let extra = (self.failures.len() - 20).min(12) as u32;
            let lock = (1000u64 << extra).min(3_600_000);
            let last = *self.failures.last().expect("non-empty");
            if now < last + lock {
                return Err(last + lock - now);
            }
        }
        v.push(now);
        if self.per_ip.len() > 10_000 {
            self.per_ip
                .retain(|_, v| v.iter().any(|t| now < t + 60_000));
        }
        Ok(())
    }
    pub fn failed(&mut self, now: u64) {
        self.failures.push(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn random_secrets_hash_cheaply_and_verify_through_the_normal_path() {
        let phc = hash_random_secret("ca71bc6b531bf53a5e27f32f787d7751acf0377219bcca90");
        // The parameters travel in the hash: verification needs no special case.
        assert!(phc.contains("m=8,t=1,p=1"), "{phc}");
        assert!(verify_password(
            "ca71bc6b531bf53a5e27f32f787d7751acf0377219bcca90",
            &phc
        ));
        assert!(!verify_password("wrong", &phc));
    }
    #[test]
    fn rate_limit() {
        let mut r = RateLimiter::default();
        let ip: IpAddr = "1.2.3.4".parse().unwrap();
        for i in 0..5 {
            assert!(r.check(ip, 1000 + i).is_ok());
        }
        assert!(r.check(ip, 2000).is_err());
        assert!(r.check(ip, 62_000).is_ok());
        let other: IpAddr = "5.6.7.8".parse().unwrap();
        for _ in 0..20 {
            r.failed(100_000);
        }
        assert!(r.check(other, 100_500).is_err(), "global lockout");
    }
    #[test]
    fn password_roundtrip() {
        let h = hash_password("hunter2");
        assert!(verify_password("hunter2", &h));
        assert!(!verify_password("hunter3", &h));
        assert_eq!(setup_code().len(), 9);
    }
}
