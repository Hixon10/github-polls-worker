use base64::{
    engine::general_purpose::STANDARD as B64_STD,
    engine::general_purpose::URL_SAFE_NO_PAD as B64URL, Engine,
};
use hmac::{Hmac, Mac};
use rsa::pkcs1v15::SigningKey;
use rsa::pkcs8::DecodePrivateKey;
use rsa::RsaPrivateKey;
use serde::Deserialize;
use sha2::Sha256;
use signature::{SignatureEncoding, Signer};
use subtle::ConstantTimeEq;
use worker::{Fetch, Headers, Method, Request, RequestInit};

use crate::errors::AppError;
use crate::session::now_secs;

const UA: &str = "github-polls-worker/0.1";

// ---------- App-level auth ----------

/// Deterministic RS256 JWT (<=10 min). PKCS#1 v1.5 => no RNG.
pub fn app_jwt(app_id: &str, pem: &str) -> Result<String, AppError> {
    let header = serde_json::json!({"alg":"RS256","typ":"JWT"});
    let iat = now_secs() as i64 - 30;
    let claims = serde_json::json!({"iat":iat,"exp":iat + 9*60,"iss":app_id});
    let h = B64URL.encode(serde_json::to_vec(&header)?);
    let c = B64URL.encode(serde_json::to_vec(&claims)?);
    let input = format!("{h}.{c}");

    let key = RsaPrivateKey::from_pkcs8_pem(pem.trim())
        .map_err(|e| AppError::Internal(format!("pkcs8: {e}")))?;
    let signer: SigningKey<Sha256> = SigningKey::new(key);
    let sig: rsa::pkcs1v15::Signature = signer.sign(input.as_bytes());
    Ok(format!("{input}.{}", B64URL.encode(sig.to_bytes())))
}

pub async fn installation_token(
    app_id: &str,
    pem: &str,
    installation_id: i64,
) -> Result<String, AppError> {
    #[derive(Deserialize)]
    struct T {
        token: String,
    }
    let jwt = app_jwt(app_id, pem)?;
    let url = format!("https://api.github.com/app/installations/{installation_id}/access_tokens");
    let t: T = gh_json(Method::Post, &url, Some(&jwt), None::<&()>).await?;
    Ok(t.token)
}

#[derive(Deserialize)]
pub struct InstallationMeta {
    pub id: i64,
    pub account: Account,
}
#[derive(Deserialize)]
pub struct Account {
    pub id: i64,
    pub login: String,
}

pub async fn get_installation(
    app_id: &str,
    pem: &str,
    installation_id: i64,
) -> Result<InstallationMeta, AppError> {
    let jwt = app_jwt(app_id, pem)?;
    let url = format!("https://api.github.com/app/installations/{installation_id}");
    gh_json(Method::Get, &url, Some(&jwt), None::<&()>).await
}

// ---------- Voter OAuth (user-to-server) ----------

#[derive(Deserialize)]
pub struct GhUser {
    pub id: i64,
    pub login: String,
}

pub async fn exchange_oauth_code(
    client_id: &str,
    client_secret: &str,
    code: &str,
    redirect_uri: &str,
) -> Result<String, AppError> {
    #[derive(Deserialize)]
    struct T {
        access_token: Option<String>,
        error: Option<String>,
    }
    let body = serde_json::json!({
        "client_id": client_id, "client_secret": client_secret,
        "code": code, "redirect_uri": redirect_uri,
    });
    let t: T = raw_json(
        Method::Post,
        "https://github.com/login/oauth/access_token",
        Some(&body),
        &[
            ("Accept", "application/json"),
            ("Content-Type", "application/json"),
            ("User-Agent", UA),
        ],
    )
    .await?;
    t.access_token.ok_or_else(|| {
        AppError::Upstream(t.error.unwrap_or_else(|| "oauth exchange failed".into()))
    })
}

pub async fn get_user(token: &str) -> Result<GhUser, AppError> {
    let bearer = format!("Bearer {token}");
    raw_json(
        Method::Get,
        "https://api.github.com/user",
        None::<&()>,
        &[
            ("Authorization", bearer.as_str()),
            ("Accept", "application/vnd.github+json"),
            ("User-Agent", UA),
            ("X-GitHub-Api-Version", "2022-11-28"),
        ],
    )
    .await
}

// ---------- Contents API ----------

pub async fn get_contents(
    token: &str,
    owner: &str,
    repo: &str,
    path: &str,
) -> Result<Option<(Vec<u8>, String)>, AppError> {
    let url = format!(
        "https://api.github.com/repos/{}/{}/contents/{}",
        owner,
        repo,
        urlencoding::encode(path)
    );
    let (status, text) = raw_text(Method::Get, &url, None::<&()>, &gh_headers(token)).await?;
    if status == 404 {
        return Ok(None);
    }
    if status >= 300 {
        return Err(AppError::Upstream(format!("get_contents {status}: {text}")));
    }
    #[derive(Deserialize)]
    struct R {
        content: String,
        encoding: String,
        sha: String,
    }
    let r: R = serde_json::from_str(&text)?;
    if r.encoding != "base64" {
        return Err(AppError::Upstream("bad encoding".into()));
    }
    let cleaned: String = r.content.chars().filter(|c| !c.is_whitespace()).collect();
    Ok(Some((
        B64_STD
            .decode(cleaned)
            .map_err(|e| AppError::Upstream(e.to_string()))?,
        r.sha,
    )))
}

pub async fn put_contents(
    token: &str,
    owner: &str,
    repo: &str,
    path: &str,
    message: &str,
    content: &[u8],
    sha: Option<&str>,
    branch: Option<&str>,
) -> Result<String, AppError> {
    let url = format!(
        "https://api.github.com/repos/{}/{}/contents/{}",
        owner,
        repo,
        urlencoding::encode(path)
    );
    let mut body = serde_json::json!({
        "message": message,
        "content": B64_STD.encode(content),
    });
    if let Some(s) = sha {
        body["sha"] = s.into();
    }
    if let Some(b) = branch {
        body["branch"] = b.into();
    }

    let (status, text) = raw_text(Method::Put, &url, Some(&body), &gh_headers(token)).await?;
    // 409/422 with sha mismatch => caller should retry.
    if status == 409 || status == 422 {
        return Err(AppError::Conflict(format!("commit conflict: {text}")));
    }
    if status >= 300 {
        return Err(AppError::Upstream(format!("put_contents {status}: {text}")));
    }
    let v: serde_json::Value = serde_json::from_str(&text)?;
    Ok(v["content"]["sha"].as_str().unwrap_or_default().to_string())
}

fn gh_headers(token: &str) -> [(&str, String); 4] {
    [
        ("Authorization", format!("Bearer {token}")),
        ("Accept", "application/vnd.github+json".to_string()),
        ("User-Agent", UA.to_string()),
        ("X-GitHub-Api-Version", "2022-11-28".to_string()),
    ]
}

// ---------- Webhook ----------

pub fn verify_webhook_sig(secret: &[u8], body: &[u8], header: Option<&str>) -> bool {
    let Some(h) = header else { return false };
    let Some(hex) = h.strip_prefix("sha256=") else {
        return false;
    };
    let Ok(expected) = hex_decode(hex) else {
        return false;
    };
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(secret).unwrap();
    mac.update(body);
    mac.finalize().into_bytes().ct_eq(&expected).unwrap_u8() == 1
}
fn hex_decode(s: &str) -> Result<Vec<u8>, ()> {
    if s.len() % 2 != 0 {
        return Err(());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| ()))
        .collect()
}

// ---------- tiny HTTP helpers ----------

async fn raw_text<B: serde::Serialize + ?Sized, H: AsHeader>(
    method: Method,
    url: &str,
    body: Option<&B>,
    headers: &[H],
) -> Result<(u16, String), AppError> {
    let mut h = Headers::new();
    for kv in headers {
        let (k, v) = kv.kv();
        h.set(k, v)?;
    }
    let mut init = RequestInit::new();
    init.with_method(method).with_headers(h);
    if let Some(b) = body {
        init.with_body(Some(serde_json::to_string(b)?.into()));
    }
    let req = Request::new_with_init(url, &init)?;
    let mut resp = Fetch::Request(req).send().await?;
    let status = resp.status_code();
    let text = resp.text().await.unwrap_or_default();
    Ok((status, text))
}

async fn raw_json<B, T, H>(
    method: Method,
    url: &str,
    body: Option<&B>,
    headers: &[H],
) -> Result<T, AppError>
where
    B: serde::Serialize + ?Sized,
    T: serde::de::DeserializeOwned,
    H: AsHeader,
{
    let (status, text) = raw_text(method, url, body, headers).await?;
    if status >= 300 {
        return Err(AppError::Upstream(format!("{url} -> {status}: {text}")));
    }
    Ok(serde_json::from_str(&text)?)
}

async fn gh_json<T, B>(
    method: Method,
    url: &str,
    bearer: Option<&str>,
    body: Option<&B>,
) -> Result<T, AppError>
where
    T: serde::de::DeserializeOwned,
    B: serde::Serialize + ?Sized,
{
    let auth = bearer.map(|t| format!("Bearer {t}"));
    let mut hdrs: Vec<(&str, String)> = vec![
        ("Accept", "application/vnd.github+json".into()),
        ("User-Agent", UA.into()),
        ("X-GitHub-Api-Version", "2022-11-28".into()),
    ];
    if let Some(a) = auth {
        hdrs.push(("Authorization", a));
    }
    raw_json(method, url, body, &hdrs).await
}

// Accept either (&str,&str) or (&str,String) headers ergonomically.
trait AsHeader {
    fn kv(&self) -> (&str, &str);
}
impl<'a, 'b> AsHeader for (&'a str, &'b str) {
    fn kv(&self) -> (&str, &str) {
        (self.0, self.1)
    }
}
impl<'a> AsHeader for (&'a str, String) {
    fn kv(&self) -> (&str, &str) {
        (self.0, self.1.as_str())
    }
}
