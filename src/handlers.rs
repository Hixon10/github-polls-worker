use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;
use worker::*;

use crate::{db, errors::AppError, github, session as sess, views};

// ---------- env helpers ----------

fn v(env: &Env, k: &str) -> Result<String> {
    env.var(k).map(|x| x.to_string())
}
fn s(env: &Env, k: &str) -> Result<String> {
    env.secret(k).map(|x| x.to_string())
}
fn session_key(env: &Env) -> std::result::Result<Vec<u8>, AppError> {
    let raw = s(env, "SESSION_HMAC_KEY").map_err(|e| AppError::Internal(e.to_string()))?;
    B64.decode(raw.trim())
        .map_err(|e| AppError::Internal(format!("SESSION_HMAC_KEY: {e}")))
}

fn voter_hmac_key(env: &Env) -> std::result::Result<Vec<u8>, AppError> {
    let raw = s(env, "VOTER_ID_HMAC_KEY").map_err(|e| AppError::Internal(e.to_string()))?;
    B64.decode(raw.trim())
        .map_err(|e| AppError::Internal(format!("VOTER_ID_HMAC_KEY: {e}")))
}

fn to_resp<E: Into<AppError>>(r: std::result::Result<Response, E>) -> Result<Response> {
    match r {
        Ok(x) => Ok(x),
        Err(e) => e.into().to_response(),
    }
}
fn html(body: String) -> Result<Response> {
    let mut resp = Response::from_html(body)?;
    resp.headers_mut().set("Cache-Control", "no-store")?;
    resp.headers_mut()
        .set("X-Content-Type-Options", "nosniff")?;
    resp.headers_mut().set("Referrer-Policy", "no-referrer")?;
    Ok(resp)
}
fn redirect(url: &str) -> Result<Response> {
    Response::redirect(Url::parse(url)?)
}

fn rand_token() -> String {
    let mut b = [0u8; 32];
    getrandom::getrandom(&mut b).unwrap();
    B64.encode(b)
}

// ---------- routes ----------

pub async fn index(_req: Request, _ctx: RouteContext<()>) -> Result<Response> {
    html(views::index_html())
}

pub async fn new_poll_form(_req: Request, _ctx: RouteContext<()>) -> Result<Response> {
    html(views::new_poll_form_html(None))
}

pub async fn create_poll(mut req: Request, ctx: RouteContext<()>) -> Result<Response> {
    to_resp(create_poll_inner(&mut req, &ctx).await)
}
async fn create_poll_inner(
    req: &mut Request,
    ctx: &RouteContext<()>,
) -> std::result::Result<Response, AppError> {
    let form = req.form_data().await?;
    let question = match form.get("question") {
        Some(FormEntry::Field(v)) => v,
        _ => String::new(),
    };
    let repo = match form.get("repo") {
        Some(FormEntry::Field(v)) => v,
        _ => String::new(),
    };
    let question = question.trim().to_string();
    let repo = repo.trim().to_string();

    if question.is_empty() || question.len() > 500 {
        return Ok(Response::from_html(views::new_poll_form_html(Some(
            "Question required (max 500 chars).",
        )))
        .unwrap());
    }
    let (owner, name) = repo
        .split_once('/')
        .ok_or_else(|| AppError::BadRequest("Repo must be owner/name".into()))?;
    if !is_slug(owner) || !is_slug(name) {
        return Err(AppError::BadRequest("Invalid repo owner/name".into()));
    }

    let env = &ctx.env;
    // Do we already know the installation for this owner?
    if let Some(inst) = db::installation_by_login(env, owner).await? {
        if inst.suspended_at.is_some() {
            return Err(AppError::Forbidden);
        }
        let poll_url = finalize_poll(env, inst.installation_id, owner, name, &question).await?;
        let commit_url = format!(
            "https://github.com/{owner}/{name}/blob/main/polls/{}.csv",
            poll_url.rsplit('/').next().unwrap()
        );
        return Ok(Response::from_html(views::poll_created_html(
            &poll_url,
            &commit_url,
        ))?);
    }

    // Otherwise bounce through App install, remembering what they wanted.
    let state = rand_token();
    db::insert_pending(env, &state, owner, name, &question).await?;
    let slug = v(env, "GITHUB_APP_SLUG").map_err(|e| AppError::Internal(e.to_string()))?;
    let url = format!(
        "https://github.com/apps/{slug}/installations/new?state={}",
        urlencoding::encode(&state)
    );
    Ok(Response::redirect(
        Url::parse(&url).map_err(|e| AppError::Internal(e.to_string()))?,
    )?)
}

pub async fn install_callback(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    to_resp(install_callback_inner(req, ctx).await)
}
async fn install_callback_inner(
    req: Request,
    ctx: RouteContext<()>,
) -> std::result::Result<Response, AppError> {
    let url = req.url()?;
    let q: std::collections::HashMap<String, String> = url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();

    let installation_id: i64 = q
        .get("installation_id")
        .and_then(|x| x.parse().ok())
        .ok_or_else(|| AppError::BadRequest("installation_id missing".into()))?;
    let setup_action = q.get("setup_action").cloned().unwrap_or_default();
    let state = q.get("state").cloned();

    let env = &ctx.env;
    let app_id = v(env, "GITHUB_APP_ID")?;
    let pem = s(env, "GITHUB_APP_PRIVATE_KEY")?;

    // Authenticate the installation (also proves installation_id is valid & ours).
    let inst = github::get_installation(&app_id, &pem, installation_id).await?;
    db::upsert_installation(env, inst.id, &inst.account.login, inst.account.id).await?;

    if setup_action == "request" {
        return Ok(Response::from_html(views::error_html(
            "Install is pending admin approval. Once approved, create the poll again.",
        ))?);
    }

    // If we have a pending poll that matches the owner who just installed us, finalize it.
    if let Some(st) = state {
        if let Some(p) = db::consume_pending(env, &st).await? {
            if p.repo_owner.eq_ignore_ascii_case(&inst.account.login) {
                let poll_url =
                    finalize_poll(env, inst.id, &p.repo_owner, &p.repo_name, &p.question).await?;
                return Ok(Response::from_html(views::install_ok_html(&poll_url))?);
            }
            // Owner mismatch – safer to tell the user.
            return Ok(Response::from_html(views::error_html(
                "The installation account doesn't match the repository owner you entered. \
                 Install the App on the correct account and try again.",
            ))?);
        }
    }

    Ok(Response::from_html(views::install_ok_html(
        "(no pending poll)",
    ))?)
}

/// Creates the CSV file + the DB row. Idempotent: existing file is left intact.
async fn finalize_poll(
    env: &Env,
    installation_id: i64,
    owner: &str,
    name: &str,
    question: &str,
) -> std::result::Result<String, AppError> {
    let app_id = v(env, "GITHUB_APP_ID")?;
    let pem = s(env, "GITHUB_APP_PRIVATE_KEY")?;
    let token = github::installation_token(&app_id, &pem, installation_id).await?;

    let id = Uuid::new_v4().to_string();
    let csv_path = format!("polls/{id}.csv");
    let header = format!(
        "# question: {}\ntimestamp,voter_id_hash,response\n",
        csv_line_comment_safe(question)
    );

    // If something pre-existed at this path, fail loudly – we never overwrite.
    if github::get_contents(&token, owner, name, &csv_path)
        .await?
        .is_some()
    {
        return Err(AppError::Conflict("csv already exists".into()));
    }
    github::put_contents(
        &token,
        owner,
        name,
        &csv_path,
        &format!("chore(polls): create {id}"),
        header.as_bytes(),
        None,
        Some("main"),
    )
    .await?;

    db::insert_poll(
        env,
        &db::Poll {
            id: id.clone(),
            installation_id,
            repo_owner: owner.to_string(),
            repo_name: name.to_string(),
            csv_path,
            question: question.to_string(),
        },
    )
    .await?;

    let base = v(env, "PUBLIC_BASE_URL")?;
    Ok(format!("{base}/p/{id}"))
}

// ---------- voter routes ----------

pub async fn poll_page(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    to_resp(poll_page_inner(req, ctx).await)
}
async fn poll_page_inner(
    req: Request,
    ctx: RouteContext<()>,
) -> std::result::Result<Response, AppError> {
    let id = ctx.param("id").cloned().unwrap_or_default();
    let env = &ctx.env;
    let poll = db::get_poll(env, &id).await?.ok_or(AppError::NotFound)?;

    let base = v(env, "PUBLIC_BASE_URL")?;
    let key = session_key(env)?;

    let Some(tok) = sess::read_cookie(&req, sess::VOTER_COOKIE) else {
        return Ok(Response::from_html(views::login_required_html(
            &format!("{base}/p/{}/login", poll.id),
            &poll.question,
        ))?);
    };
    let voter = match sess::verify(&tok, &key) {
        Ok(v) => v,
        Err(_) => {
            return Ok(Response::from_html(views::login_required_html(
                &format!("{base}/p/{}/login", poll.id),
                &poll.question,
            ))?);
        }
    };

    // Has the voter already voted? Read CSV and look for their hash.
    let app_id = v(env, "GITHUB_APP_ID")?;
    let pem = s(env, "GITHUB_APP_PRIVATE_KEY")?;
    let token = github::installation_token(&app_id, &pem, poll.installation_id).await?;
    let (bytes, _sha) =
        github::get_contents(&token, &poll.repo_owner, &poll.repo_name, &poll.csv_path)
            .await?
            .ok_or(AppError::NotFound)?;
    let vkey = voter_hmac_key(env)?;
    let hash = voter_hash(&vkey, &poll.id, voter.uid);
    let already = csv_has_hash(&bytes, &hash);

    Ok(Response::from_html(views::poll_vote_html(
        &poll.question,
        &format!("{base}/p/{}/vote", poll.id),
        &voter.login,
        already,
    ))?)
}

pub async fn voter_login(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    to_resp(voter_login_inner(req, ctx).await)
}
async fn voter_login_inner(
    _req: Request,
    ctx: RouteContext<()>,
) -> std::result::Result<Response, AppError> {
    let id = ctx.param("id").cloned().unwrap_or_default();
    let env = &ctx.env;
    // Poll must exist to bind state.
    let _poll = db::get_poll(env, &id).await?.ok_or(AppError::NotFound)?;

    let state = rand_token();
    db::put_oauth_state(env, &state, &id).await?;

    let client_id = v(env, "GITHUB_APP_CLIENT_ID")?;
    let base = v(env, "PUBLIC_BASE_URL")?;
    let redirect = format!("{base}/p/callback");
    let url = format!(
        "https://github.com/login/oauth/authorize?client_id={}&redirect_uri={}&state={}&scope=&allow_signup=true",
        urlencoding::encode(&client_id),
        urlencoding::encode(&redirect),
        urlencoding::encode(&state),
    );
    Ok(Response::redirect(
        Url::parse(&url).map_err(|e| AppError::Internal(e.to_string()))?,
    )?)
}

pub async fn voter_callback(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    to_resp(voter_callback_inner(req, ctx).await)
}
async fn voter_callback_inner(
    req: Request,
    ctx: RouteContext<()>,
) -> std::result::Result<Response, AppError> {
    let env = &ctx.env;

    let url = req.url()?;
    let q: std::collections::HashMap<String, String> = url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    let code = q
        .get("code")
        .cloned()
        .ok_or_else(|| AppError::BadRequest("code missing".into()))?;
    let state = q
        .get("state")
        .cloned()
        .ok_or_else(|| AppError::BadRequest("state missing".into()))?;

    // state -> poll_id (single-use, TTL enforced in db::consume_oauth_state)
    let poll_id = db::consume_oauth_state(env, &state)
        .await?
        .ok_or(AppError::Unauthorized)?;

    let client_id = v(env, "GITHUB_APP_CLIENT_ID")?;
    let client_secret = s(env, "GITHUB_APP_CLIENT_SECRET")?;
    let base = v(env, "PUBLIC_BASE_URL")?;

    // MUST match the redirect_uri used on /authorize, byte-for-byte.
    let token = github::exchange_oauth_code(
        &client_id,
        &client_secret,
        &code,
        &format!("{base}/p/callback"),
    )
    .await?;
    let user = github::get_user(&token).await?;

    let key = session_key(env)?;
    let now = sess::now_secs();
    let sess = sess::VoterSession {
        uid: user.id,
        login: user.login,
        exp: now + sess::VOTER_TTL,
    };
    let signed = sess::sign(&sess, &key)?;

    let mut headers = worker::Headers::new();
    headers.set("Location", &format!("{base}/p/{poll_id}"))
        .map_err(|e| AppError::Internal(e.to_string()))?;
    headers.set("Cache-Control", "no-store")
        .map_err(|e| AppError::Internal(e.to_string()))?;
    sess::set_cookie(&mut headers, sess::VOTER_COOKIE, &signed, sess::VOTER_TTL);

    Ok(Response::empty()?
        .with_status(302)
        .with_headers(headers))
}

pub async fn submit_vote(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    to_resp(submit_vote_inner(req, ctx).await)
}
async fn submit_vote_inner(
    mut req: Request,
    ctx: RouteContext<()>,
) -> std::result::Result<Response, AppError> {
    let id = ctx.param("id").cloned().unwrap_or_default();
    let env = &ctx.env;

    let key = session_key(env)?;
    let tok = sess::read_cookie(&req, sess::VOTER_COOKIE).ok_or(AppError::Unauthorized)?;
    let voter = sess::verify(&tok, &key)?;

    let poll = db::get_poll(env, &id).await?.ok_or(AppError::NotFound)?;

    let form = req.form_data().await?;
    let response = match form.get("response") {
        Some(FormEntry::Field(v)) => v,
        _ => String::new(),
    };
    let response = response.trim().to_string();
    if response.is_empty() || response.len() > 200 {
        return Err(AppError::BadRequest("empty or too long response".into()));
    }

    let app_id = v(env, "GITHUB_APP_ID")?;
    let pem = s(env, "GITHUB_APP_PRIVATE_KEY")?;
    let token = github::installation_token(&app_id, &pem, poll.installation_id).await?;

    let vkey = voter_hmac_key(env)?;
    let vhash = voter_hash(&vkey, &poll.id, voter.uid);
    let ts = sess::now_secs();
    let row = format!("{ts},{vhash},{}\n", csv_quote(&response));

    // Optimistic concurrency: read sha, append, PUT with sha. Retry a couple of times on 409.
    for attempt in 0..3u8 {
        let (bytes, sha) =
            github::get_contents(&token, &poll.repo_owner, &poll.repo_name, &poll.csv_path)
                .await?
                .ok_or(AppError::NotFound)?;

        if csv_has_hash(&bytes, &vhash) {
            return Ok(Response::redirect(github_blob_url(&poll))?);
        }

        let mut new_bytes = bytes.clone();
        if !new_bytes.ends_with(b"\n") {
            new_bytes.push(b'\n');
        }
        new_bytes.extend_from_slice(row.as_bytes());

        match github::put_contents(
            &token,
            &poll.repo_owner,
            &poll.repo_name,
            &poll.csv_path,
            &format!("vote: {}", &poll.id),
            &new_bytes,
            Some(&sha),
            Some("main"),
        )
        .await
        {
            Ok(_) => return Ok(Response::redirect(github_blob_url(&poll))?),
            Err(AppError::Conflict(_)) if attempt < 2 => continue,
            Err(e) => return Err(e),
        }
    }
    Err(AppError::Conflict(
        "could not commit vote after retries".into(),
    ))
}

fn github_blob_url(poll: &db::Poll) -> Url {
    Url::parse(&format!(
        "https://github.com/{}/{}/blob/main/{}",
        poll.repo_owner, poll.repo_name, poll.csv_path
    ))
    .unwrap()
}

// ---------- webhook ----------

pub async fn github_webhook(mut req: Request, ctx: RouteContext<()>) -> Result<Response> {
    to_resp(webhook_inner(&mut req, &ctx).await)
}
async fn webhook_inner(
    req: &mut Request,
    ctx: &RouteContext<()>,
) -> std::result::Result<Response, AppError> {
    let env = &ctx.env;
    let secret = s(env, "GITHUB_WEBHOOK_SECRET")?;
    let sig = req.headers().get("x-hub-signature-256").ok().flatten();
    let event = req
        .headers()
        .get("x-github-event")
        .ok()
        .flatten()
        .unwrap_or_default();

    let body = req.bytes().await?;
    if !github::verify_webhook_sig(secret.as_bytes(), &body, sig.as_deref()) {
        return Err(AppError::Unauthorized);
    }

    if event == "installation" || event == "installation_repositories" {
        #[derive(Deserialize)]
        struct Acct {
            id: i64,
            login: String,
        }
        #[derive(Deserialize)]
        struct Inst {
            id: i64,
            account: Acct,
        }
        #[derive(Deserialize)]
        struct Payload {
            action: String,
            installation: Inst,
        }
        let p: Payload = serde_json::from_slice(&body)?;
        match p.action.as_str() {
            "created" | "new_permissions_accepted" | "added" => {
                db::upsert_installation(
                    env,
                    p.installation.id,
                    &p.installation.account.login,
                    p.installation.account.id,
                )
                .await?
            }
            "suspend" => db::mark_installation_suspended(env, p.installation.id, true).await?,
            "unsuspend" => db::mark_installation_suspended(env, p.installation.id, false).await?,
            "deleted" => db::delete_installation(env, p.installation.id).await?,
            _ => {}
        }
    }
    Ok(Response::ok("")?)
}

// ---------- tiny helpers ----------

fn is_slug(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn voter_hash(key: &[u8], poll_id: &str, uid: i64) -> String {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC key");
    mac.update(poll_id.as_bytes());
    mac.update(b"\0");
    mac.update(uid.to_string().as_bytes());
    hex(&mac.finalize().into_bytes())
}

fn hex(b: &[u8]) -> String {
    const H: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(b.len() * 2);
    for &x in b {
        s.push(H[(x >> 4) as usize] as char);
        s.push(H[(x & 0xF) as usize] as char);
    }
    s
}

/// True if any CSV line has `hash` as its second field.
fn csv_has_hash(bytes: &[u8], hash: &str) -> bool {
    for line in bytes.split(|&b| b == b'\n') {
        if line.is_empty() || line.starts_with(b"#") || line.starts_with(b"timestamp,") {
            continue;
        }
        let s = match std::str::from_utf8(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let mut cols = s.splitn(3, ',');
        let _ts = cols.next();
        if cols.next() == Some(hash) {
            return true;
        }
    }
    false
}

fn csv_quote(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}
fn csv_line_comment_safe(s: &str) -> String {
    s.replace(['\n', '\r'], " ")
}
