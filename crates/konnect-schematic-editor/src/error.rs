use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Parse error at byte {pos}: {msg}")]
    Parse { pos: usize, msg: String },

    #[error("Missing required field '{0}'")]
    MissingField(&'static str),
}

pub type Result<T> = std::result::Result<T, Error>;
