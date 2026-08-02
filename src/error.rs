pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
  #[error("{0}")]
  Validate(String),
  #[error("{0}")]
  Runner(String),
  #[error(transparent)]
  Io(#[from] std::io::Error),
  #[error(transparent)]
  Yaml(#[from] serde_norway::Error),
  #[error(transparent)]
  Json(#[from] serde_json::Error),
}
