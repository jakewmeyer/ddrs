use thiserror::Error;

/// Crate error type
#[derive(Error, Debug)]
pub enum Error {
    #[error("STUN error")]
    Stun(#[from] stun::Error),

    #[error("IO error")]
    Io(#[from] std::io::Error),

    #[error("Request error")]
    Request(#[from] reqwest::Error),

    #[error("Local IP address error")]
    LocalIpAddress(#[from] local_ip_address::Error),

    #[error("Unknown error")]
    Unknown,
}
