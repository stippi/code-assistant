use thiserror::Error;

#[derive(Error, Debug)]
pub enum ProxyError {
    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Authentication error: {0}")]
    Auth(String),

    #[error("Keyring error: {0}")]
    Keyring(#[from] keyring::Error),

    #[error("HTTP client error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("HTTP error: {0}")]
    HttpGeneral(String),

    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("OAuth2 error: {0}")]
    OAuth(#[from] oauth2::basic::BasicRequestTokenError<oauth2::reqwest::Error<reqwest::Error>>),

    #[error("URL parse error: {0}")]
    UrlParse(#[from] oauth2::url::ParseError),

    #[error("Base64 decode error: {0}")]
    Base64(#[from] base64::DecodeError),

    #[error("UTF-8 decode error: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),
}

pub type Result<T> = std::result::Result<T, ProxyError>;
