use thiserror::Error;

#[derive(Debug, Error)]
pub enum PypiClientError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("HTML parse error: {0}")]
    HtmlParse(String),

    #[error("Metadata parse error: {0}")]
    MetadataParse(String),

    #[error("Invalid wheel filename: {0}")]
    InvalidWheelFilename(String),

    #[error("Cache I/O error: {0}")]
    Cache(#[from] std::io::Error),

    #[error("URL parse error: {0}")]
    Url(#[from] url::ParseError),

    #[error("Unexpected content type: {0}")]
    UnexpectedContentType(String),

    #[error("Request failed after {attempts} attempts: {message}")]
    RetryExhausted { attempts: u32, message: String },
}

pub type Result<T> = std::result::Result<T, PypiClientError>;
