use serde::Deserialize;
use worker::wasm_bindgen::JsValue;
use worker::{D1Database, Env};

use crate::errors::AppError;
use crate::session::now_secs;

pub fn db(env: &Env) -> Result<D1Database, AppError> {
    env.d1("DB").map_err(|e| AppError::Internal(e.to_string()))
}

// ---------- D1 binding helpers ----------
//
// D1's JS driver rejects `BigInt` parameters with `D1_TYPE_ERROR`. wasm-bindgen
// maps Rust `i64 -> BigInt`, so we must down-cast integers to `f64` before
// binding. All GitHub IDs and unix timestamps we use are < 2^53, so no loss.

#[inline]
fn n(x: i64) -> JsValue {
    JsValue::from_f64(x as f64)
}
#[inline]
fn t(s: &str) -> JsValue {
    JsValue::from_str(s)
}
#[inline]
fn n_opt(x: Option<i64>) -> JsValue {
    match x {
        Some(v) => JsValue::from_f64(v as f64),
        None => JsValue::NULL,
    }
}
#[inline]
fn now_js() -> JsValue {
    n(now_secs() as i64)
}

// ---------- models ----------

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)] // account_login/account_id kept for logs & future use
pub struct Installation {
    pub installation_id: i64,
    pub account_login: String,
    pub account_id: i64,
    pub suspended_at: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Poll {
    pub id: String,
    pub installation_id: i64,
    pub repo_owner: String,
    pub repo_name: String,
    pub csv_path: String,
    pub question: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)] // created_at kept for TTL cleanup job
pub struct Pending {
    pub repo_owner: String,
    pub repo_name: String,
    pub question: String,
    pub created_at: i64,
}

// ---------- installations ----------

pub async fn upsert_installation(
    env: &Env,
    id: i64,
    login: &str,
    account_id: i64,
) -> Result<(), AppError> {
    db(env)?
        .prepare(
            "INSERT INTO installations(installation_id, account_login, account_id, created_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(installation_id) DO UPDATE SET
                account_login = excluded.account_login,
                account_id    = excluded.account_id,
                suspended_at  = NULL",
        )
        .bind(&[n(id), t(login), n(account_id), now_js()])?
        .run()
        .await?;
    Ok(())
}

pub async fn mark_installation_suspended(
    env: &Env,
    id: i64,
    suspended: bool,
) -> Result<(), AppError> {
    let ts = if suspended {
        Some(now_secs() as i64)
    } else {
        None
    };
    db(env)?
        .prepare("UPDATE installations SET suspended_at = ?1 WHERE installation_id = ?2")
        .bind(&[n_opt(ts), n(id)])?
        .run()
        .await?;
    Ok(())
}

pub async fn delete_installation(env: &Env, id: i64) -> Result<(), AppError> {
    db(env)?
        .prepare("DELETE FROM installations WHERE installation_id = ?1")
        .bind(&[n(id)])?
        .run()
        .await?;
    Ok(())
}

pub async fn installation_by_login(
    env: &Env,
    login: &str,
) -> Result<Option<Installation>, AppError> {
    let row = db(env)?
        .prepare(
            "SELECT installation_id, account_login, account_id, suspended_at
             FROM installations WHERE lower(account_login) = lower(?1) LIMIT 1",
        )
        .bind(&[t(login)])?
        .first::<Installation>(None)
        .await?;
    Ok(row)
}

// ---------- pending polls ----------

pub async fn insert_pending(
    env: &Env,
    state: &str,
    owner: &str,
    repo: &str,
    question: &str,
) -> Result<(), AppError> {
    db(env)?
        .prepare(
            "INSERT INTO pending_polls(state, repo_owner, repo_name, question, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .bind(&[t(state), t(owner), t(repo), t(question), now_js()])?
        .run()
        .await?;
    Ok(())
}

pub async fn consume_pending(env: &Env, state: &str) -> Result<Option<Pending>, AppError> {
    let d = db(env)?;
    let row = d
        .prepare(
            "SELECT repo_owner, repo_name, question, created_at
             FROM pending_polls WHERE state = ?1",
        )
        .bind(&[t(state)])?
        .first::<Pending>(None)
        .await?;
    if row.is_some() {
        d.prepare("DELETE FROM pending_polls WHERE state = ?1")
            .bind(&[t(state)])?
            .run()
            .await?;
    }
    Ok(row)
}

// ---------- polls ----------

pub async fn insert_poll(env: &Env, p: &Poll) -> Result<(), AppError> {
    db(env)?
        .prepare(
            "INSERT INTO polls(id, installation_id, repo_owner, repo_name, csv_path,
                               question, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )
        .bind(&[
            t(&p.id),
            n(p.installation_id),
            t(&p.repo_owner),
            t(&p.repo_name),
            t(&p.csv_path),
            t(&p.question),
            now_js(),
        ])?
        .run()
        .await?;
    Ok(())
}

pub async fn get_poll(env: &Env, id: &str) -> Result<Option<Poll>, AppError> {
    Ok(db(env)?
        .prepare(
            "SELECT id, installation_id, repo_owner, repo_name, csv_path, question
             FROM polls WHERE id = ?1",
        )
        .bind(&[t(id)])?
        .first::<Poll>(None)
        .await?)
}

// ---------- oauth state ----------

pub async fn put_oauth_state(env: &Env, state: &str, poll: &str) -> Result<(), AppError> {
    db(env)?
        .prepare("INSERT INTO oauth_states(state, poll_id, created_at) VALUES (?1, ?2, ?3)")
        .bind(&[t(state), t(poll), now_js()])?
        .run()
        .await?;
    Ok(())
}

pub async fn consume_oauth_state(env: &Env, state: &str) -> Result<Option<String>, AppError> {
    let d = db(env)?;
    let row: Option<serde_json::Value> = d
        .prepare("SELECT poll_id, created_at FROM oauth_states WHERE state = ?1")
        .bind(&[t(state)])?
        .first(None)
        .await?;
    if row.is_some() {
        d.prepare("DELETE FROM oauth_states WHERE state = ?1")
            .bind(&[t(state)])?
            .run()
            .await?;
    }
    Ok(row.and_then(|v| {
        let age = now_secs() as i64 - v.get("created_at").and_then(|x| x.as_i64()).unwrap_or(0);
        if age > 600 {
            return None;
        }
        v.get("poll_id").and_then(|x| x.as_str()).map(String::from)
    }))
}
