use thiserror::Error;

#[derive(Debug, Error)]
pub enum DiscoveryError {
    #[error("gRPC connect/subscribe error: {0}")]
    Grpc(String),
}
