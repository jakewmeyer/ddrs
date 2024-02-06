use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("STUN error")]
    Stun(#[from] stun::Error),

    #[error("IO error")]
    Io(#[from] std::io::Error),

    #[error("Request error")]
    Request(#[from] reqwest::Error),

    #[error("Unknown error")]
    Unknown,
}
