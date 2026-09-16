use std::borrow::Cow;

pub const ARES_ABI_VERSION: i32 = 1;

pub const ARES_OK: i32 = 0;
pub const ARES_STATUS_TRUNCATED: i32 = 1;

pub const ARES_ERROR_NULL_POINTER: i32 = -1;
pub const ARES_ERROR_INVALID_LENGTH: i32 = -2;
pub const ARES_ERROR_BACKEND: i32 = -3;

#[derive(Debug)]
pub enum AresError {
    NullPointer,
    InvalidLength(Cow<'static, str>),
    Backend(Cow<'static, str>),
}

impl AresError {
    pub fn invalid_length(message: impl Into<Cow<'static, str>>) -> Self {
        Self::InvalidLength(message.into())
    }

    pub fn backend(message: impl Into<Cow<'static, str>>) -> Self {
        Self::Backend(message.into())
    }

    pub fn code(&self) -> i32 {
        match self {
            Self::NullPointer => ARES_ERROR_NULL_POINTER,
            Self::InvalidLength(_) => ARES_ERROR_INVALID_LENGTH,
            Self::Backend(_) => ARES_ERROR_BACKEND,
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Self::NullPointer => "null output pointer",
            Self::InvalidLength(message) => message,
            Self::Backend(message) => message,
        }
    }
}
