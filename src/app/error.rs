use base64::DecodeError;
use reqwest::Error as ReqwestError;
use serde_json::Error as SerdeJsonError;
use solana_client::client_error::ClientError as RpcClientError;
use solana_sdk::{message::CompileError, signer::SignerError};
use std::io::Error as IoError;
use tokio::sync::AcquireError as SemaphoreAcquireError;

#[derive(Debug)]
pub enum AppError {
    CompileError(CompileError),
    IoError(IoError),
    LiquidateIxBuilderError(String),
    LiquidateMarginfiAccountMode,
    MissingCacheData,
    MissingMarginfiAccount,
    ParsingError(String),
    ReqwestError(ReqwestError),
    RpcClientError(RpcClientError),
    SemaphoreAcquireError(SemaphoreAcquireError),
    SerdeJsonError(SerdeJsonError),
    SwitchboardInvalidAccount,
    SignerError(SignerError),
    // SolanaClientReqwestError(SolanaClientReqwestError),
    TransactionTooLarge(usize),
}

impl From<CompileError> for AppError {
    fn from(value: CompileError) -> Self {
        AppError::CompileError(value)
    }
}

impl From<DecodeError> for AppError {
    fn from(value: DecodeError) -> Self {
        AppError::ParsingError(format!("{value}"))
    }
}

impl From<IoError> for AppError {
    fn from(value: IoError) -> Self {
        AppError::IoError(value)
    }
}

impl From<ReqwestError> for AppError {
    fn from(value: ReqwestError) -> Self {
        AppError::ReqwestError(value)
    }
}

impl From<RpcClientError> for AppError {
    fn from(value: RpcClientError) -> Self {
        AppError::RpcClientError(value)
    }
}

impl From<SemaphoreAcquireError> for AppError {
    fn from(value: SemaphoreAcquireError) -> Self {
        AppError::SemaphoreAcquireError(value)
    }
}

impl From<SerdeJsonError> for AppError {
    fn from(value: SerdeJsonError) -> Self {
        AppError::SerdeJsonError(value)
    }
}

impl From<SignerError> for AppError {
    fn from(value: SignerError) -> Self {
        AppError::SignerError(value)
    }
}

pub type AppResult<T> = Result<T, AppError>;
