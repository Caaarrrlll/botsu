use crate::adapter::Platform;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The platform (or our adapter for it) can't do this yet.
    #[error("{op} is not supported on {platform}")]
    Unsupported {
        platform: Platform,
        op: &'static str,
    },

    /// The platform rejected or failed the request. Adapters wrap their
    /// library's error into this so the core never sees platform types.
    #[error("{platform} error: {message}")]
    Platform { platform: Platform, message: String },

    /// Bad user input to a command; the message is shown to the user.
    #[error("{0}")]
    Usage(String),

    /// Something on our side failed (database, outside service). Logged; the user
    /// sees a generic message.
    #[error("{0}")]
    Internal(String),
}

impl Error {
    pub fn platform(platform: Platform, err: impl std::fmt::Display) -> Self {
        Self::Platform {
            platform,
            message: err.to_string(),
        }
    }

    pub fn internal(context: &str, err: impl std::fmt::Display) -> Self {
        Self::Internal(format!("{context}: {err}"))
    }
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Self::internal("database", e)
    }
}
