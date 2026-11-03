use serde_json::Value;

/// boat's error envelope: `{ok:false, status, code, message, error:{code, message, details}, requestId}`.
#[derive(Debug, Clone)]
pub struct ApiError {
    pub status: u16,
    pub code: String,
    pub message: String,
    pub details: Value,
    pub request_id: Option<String>,
    /// From the `Retry-After` header or `details.retryAfterSeconds` (429/503).
    pub retry_after_seconds: Option<u64>,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "boat {} {}: {}", self.status, self.code, self.message)?;
        if let Some(r) = &self.request_id {
            write!(f, " ({r})")?;
        }
        Ok(())
    }
}

/// An error returned by boat, or a failure to reach it.
#[derive(Debug, thiserror::Error)]
pub enum BoatError {
    /// boat answered with `ok:false` (or a non-2xx status).
    #[error("{0}")]
    Api(Box<ApiError>),
    #[error("Couldn't reach boat: {0}")]
    Http(String),
    #[error("Unexpected answer from boat: {0}")]
    Decode(String),
}

impl BoatError {
    pub fn api(&self) -> Option<&ApiError> {
        match self {
            BoatError::Api(a) => Some(a),
            _ => None,
        }
    }

    /// True when this is an API error with the given `code`.
    pub fn is(&self, code: &str) -> bool {
        self.api().is_some_and(|a| a.code == code)
    }

    pub fn status(&self) -> Option<u16> {
        self.api().map(|a| a.status)
    }

    pub fn details(&self) -> Option<&Value> {
        self.api().map(|a| &a.details)
    }

    /// `details.reason`, e.g. `turn_not_started` on `409 no_running_turn`.
    pub fn reason(&self) -> Option<&str> {
        self.details()?.get("reason")?.as_str()
    }

    pub fn retry_after_seconds(&self) -> Option<u64> {
        self.api().and_then(|a| a.retry_after_seconds)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Boat(#[from] BoatError),
    #[error("Couldn't read Flotilla's data: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("Couldn't read data: {0}")]
    Json(#[from] serde_json::Error),
    #[error("File error: {0}")]
    Io(#[from] std::io::Error),
    #[error("no boat API key: {0}")]
    NoApiKey(String),
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    NotFound(String),
    #[error("This thread's VM is gone")]
    Expired(String),
    #[error("timed out: {0}")]
    Timeout(String),
    /// A message the user didn't send (Flotilla's own, or an agent's) didn't go: the thread is
    /// archived, or the user stopped its VM (`orchestrate::may_wake`).
    #[error("not now: {0}")]
    NotNow(String),
    /// Title or reply summaries (optional; the app works without them).
    #[error("{0}")]
    Summary(String),
    /// The summary model answered or commented instead of giving a short title, name or line.
    #[error("the summary model didn't give a usable answer: {0}")]
    NoAnswer(String),
}

impl Error {
    pub fn boat(&self) -> Option<&BoatError> {
        match self {
            Error::Boat(e) => Some(e),
            _ => None,
        }
    }

    /// True when this is a boat API error with the given `code`.
    pub fn is(&self, code: &str) -> bool {
        self.boat().is_some_and(|e| e.is(code))
    }

    /// The error for a person: boat's own words without its status, code and request id
    /// (`Display` keeps them, for logs and the CLI).
    pub fn plain(&self) -> String {
        match self.boat().and_then(BoatError::api) {
            Some(a) if !a.message.trim().is_empty() => a.message.trim().to_string(),
            _ => self.to_string(),
        }
    }
}

/// A secret kept on this device that couldn't be read or kept (`flotilla_model::secrets`).
impl From<flotilla_model::secrets::Error> for Error {
    fn from(e: flotilla_model::secrets::Error) -> Error {
        match e {
            flotilla_model::secrets::Error::Missing(s) => Error::NoApiKey(s),
            flotilla_model::secrets::Error::Invalid(s) => Error::Invalid(s),
            flotilla_model::secrets::Error::Io(e) => Error::Io(e),
        }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
