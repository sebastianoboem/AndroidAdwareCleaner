use thiserror::Error;

#[derive(Debug, Error)]
pub enum AdbError {
    #[error("adb binary not found; install Android platform-tools or place adb in resources/platform-tools")]
    BinaryNotFound,
    #[error("adb command failed: {0}")]
    CommandFailed(String),
    #[error("failed to parse adb output: {0}")]
    ParseError(String),
    #[error("no device connected")]
    NoDevice,
    #[error("device unauthorized — accept USB debugging prompt on phone")]
    Unauthorized,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
