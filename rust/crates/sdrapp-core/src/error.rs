#![forbid(unsafe_code)]

#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("device not found: {0}")]
    DeviceNotFound(String),
    #[error("frequency out of range: {0} Hz")]
    FrequencyOutOfRange(u64),
    #[error("sample rate not supported: {0} sps")]
    SampleRateNotSupported(u32),
    #[error("hardware error: {0}")]
    Hardware(String),
}

#[derive(Debug, thiserror::Error)]
pub enum SinkError {
    #[error("device not found: {0}")]
    DeviceNotFound(String),
    #[error("hardware error: {0}")]
    Hardware(String),
}
