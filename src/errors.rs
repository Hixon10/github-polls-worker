use worker::{Response, Result};

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("bad request: {0}")]  BadRequest(String),
    #[error("unauthorized")]      Unauthorized,
    #[error("forbidden")]         Forbidden,
    #[error("not found")]         NotFound,
    #[error("conflict: {0}")]     Conflict(String),
    #[error("upstream: {0}")]     Upstream(String),
    #[error("internal: {0}")]     Internal(String),
}

impl AppError {
    pub fn status(&self) -> u16 {
        match self {
            Self::BadRequest(_) => 400, Self::Unauthorized  => 401,
            Self::Forbidden     => 403, Self::NotFound      => 404,
            Self::Conflict(_)   => 409, Self::Upstream(_)   => 502,
            Self::Internal(_)   => 500,
        }
    }
    pub fn to_response(&self) -> Result<Response> {
        Response::error(self.to_string(), self.status())
    }
}

impl From<worker::Error>     for AppError { fn from(e: worker::Error)     -> Self { Self::Internal(e.to_string()) } }
impl From<serde_json::Error> for AppError { fn from(e: serde_json::Error) -> Self { Self::Internal(e.to_string()) } }