use thiserror::Error;

#[derive(Debug, Error)]
pub enum SexpError {
    #[error("Parse error at offset {offset}: {message}")]
    Parse { offset: usize, message: String },

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Missing node: {0}")]
    MissingNode(String),

    #[error("Invalid value: {0}")]
    InvalidValue(String),
}
