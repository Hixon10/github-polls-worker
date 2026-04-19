use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use subtle::ConstantTimeEq;
use worker::{Headers, Request};

use crate::errors::AppError;

pub const VOTER_COOKIE: &str = "gh_polls_voter";
pub const VOTER_TTL:    u64  = 60 * 60 * 8;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoterSession {
    pub uid:   i64,
    pub login: String,
    pub exp:   u64,
}

pub fn now_secs() -> u64 { (worker::Date::now().as_millis() / 1000) as u64 }

pub fn sign(sess: &VoterSession, key: &[u8]) -> Result<String, AppError> {
    let p64 = B64.encode(serde_json::to_vec(sess)?);
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key)
        .map_err(|_| AppError::Internal("bad hmac key".into()))?;
    mac.update(p64.as_bytes());
    Ok(format!("{}.{}", p64, B64.encode(mac.finalize().into_bytes())))
}

pub fn verify(token: &str, key: &[u8]) -> Result<VoterSession, AppError> {
    let (p, s) = token.split_once('.').ok_or(AppError::Unauthorized)?;
    let sig    = B64.decode(s).map_err(|_| AppError::Unauthorized)?;
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key)
        .map_err(|_| AppError::Internal("bad hmac key".into()))?;
    mac.update(p.as_bytes());
    if mac.finalize().into_bytes().ct_eq(&sig).unwrap_u8() != 1 {
        return Err(AppError::Unauthorized);
    }
    let sess: VoterSession = serde_json::from_slice(
        &B64.decode(p).map_err(|_| AppError::Unauthorized)?
    ).map_err(|_| AppError::Unauthorized)?;
    if sess.exp < now_secs() { return Err(AppError::Unauthorized); }
    Ok(sess)
}

pub fn read_cookie(req: &Request, name: &str) -> Option<String> {
    let h = req.headers().get("cookie").ok().flatten()?;
    for part in h.split(';') {
        let p = part.trim();
        if let Some(v) = p.strip_prefix(&format!("{name}=")) { return Some(v.to_string()); }
    }
    None
}

pub fn set_cookie(h: &mut Headers, name: &str, value: &str, max_age: u64) {
    let _ = h.append("Set-Cookie",
        &format!("{name}={value}; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age={max_age}"));
}
pub fn clear_cookie(h: &mut Headers, name: &str) {
    let _ = h.append("Set-Cookie",
        &format!("{name}=; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=0"));
}