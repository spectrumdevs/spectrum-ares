use std::ffi::c_char;
use std::ptr;
use std::slice;

use crate::dsp::bands::DEFAULT_BAND_COUNT;
use crate::error::{
    AresError, ARES_ABI_VERSION, ARES_ERROR_INVALID_LENGTH, ARES_ERROR_NULL_POINTER, ARES_OK,
    ARES_STATUS_TRUNCATED,
};
use crate::state::runtime;

#[no_mangle]
pub extern "C" fn ares_get_abi_version() -> i32 {
    ARES_ABI_VERSION
}

#[no_mangle]
pub extern "C" fn ares_start() -> i32 {
    runtime().start()
}

#[no_mangle]
pub extern "C" fn ares_stop() -> i32 {
    runtime().stop()
}

#[no_mangle]
pub extern "C" fn ares_is_running() -> i32 {
    i32::from(runtime().is_running())
}

#[no_mangle]
pub extern "C" fn ares_get_band_count() -> i32 {
    DEFAULT_BAND_COUNT as i32
}

#[no_mangle]
pub extern "C" fn ares_get_bands(out: *mut f32, len: i32) -> i32 {
    if out.is_null() {
        return runtime().set_error(AresError::NullPointer);
    }

    if len <= 0 {
        return runtime().set_error(AresError::invalid_length(
            "band buffer length must be positive",
        ));
    }

    let output = unsafe { slice::from_raw_parts_mut(out, len as usize) };
    let snapshot = runtime().snapshot();
    let copy_len = output.len().min(DEFAULT_BAND_COUNT);
    output[..copy_len].copy_from_slice(&snapshot.bands[..copy_len]);

    if copy_len < DEFAULT_BAND_COUNT {
        runtime().set_error(AresError::invalid_length(
            "band buffer is shorter than the configured band count",
        ));
        ARES_STATUS_TRUNCATED
    } else {
        ARES_OK
    }
}

#[no_mangle]
pub extern "C" fn ares_get_rms() -> f32 {
    runtime().snapshot().rms
}

#[no_mangle]
pub extern "C" fn ares_get_peak() -> f32 {
    runtime().snapshot().peak
}

#[no_mangle]
pub extern "C" fn ares_get_last_error(buffer: *mut c_char, len: usize) -> i32 {
    if buffer.is_null() {
        return ARES_ERROR_NULL_POINTER;
    }

    if len == 0 {
        return ARES_ERROR_INVALID_LENGTH;
    }

    let message = runtime().last_error();
    let bytes = message.as_bytes();
    let copy_len = bytes.len().min(len.saturating_sub(1));

    unsafe {
        ptr::copy_nonoverlapping(bytes.as_ptr(), buffer.cast::<u8>(), copy_len);
        *buffer.add(copy_len) = 0;
    }

    if copy_len < bytes.len() {
        ARES_STATUS_TRUNCATED
    } else {
        ARES_OK
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CStr;
    use std::sync::Mutex;
    use std::thread;
    use std::time::Duration;

    static TEST_RUNTIME_LOCK: Mutex<()> = Mutex::new(());

    fn with_test_runtime(test: impl FnOnce()) {
        let _guard = TEST_RUNTIME_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let _ = ares_stop();
        let previous_backend = std::env::var("SPECTRUM_ARES_BACKEND").ok();
        std::env::set_var("SPECTRUM_ARES_BACKEND", "mock");
        test();
        match previous_backend {
            Some(value) => std::env::set_var("SPECTRUM_ARES_BACKEND", value),
            None => std::env::remove_var("SPECTRUM_ARES_BACKEND"),
        }
        let _ = ares_stop();
    }

    #[test]
    fn abi_version_is_current() {
        assert_eq!(ares_get_abi_version(), 1);
    }

    #[test]
    fn start_stop_lifecycle_is_idempotent() {
        with_test_runtime(|| {
            assert_eq!(ares_is_running(), 0);

            assert_eq!(ares_start(), ARES_OK);
            assert_eq!(ares_is_running(), 1);
            assert_eq!(ares_start(), ARES_OK);
            assert_eq!(ares_is_running(), 1);

            assert_eq!(ares_stop(), ARES_OK);
            assert_eq!(ares_is_running(), 0);
            assert_eq!(ares_stop(), ARES_OK);
        });
    }

    #[test]
    fn get_bands_returns_moving_mock_values() {
        with_test_runtime(|| {
            assert_eq!(ares_start(), ARES_OK);
            thread::sleep(Duration::from_millis(80));

            let mut first = [0.0_f32; DEFAULT_BAND_COUNT];
            assert_eq!(
                ares_get_bands(first.as_mut_ptr(), first.len() as i32),
                ARES_OK
            );

            thread::sleep(Duration::from_millis(80));

            let mut second = [0.0_f32; DEFAULT_BAND_COUNT];
            assert_eq!(
                ares_get_bands(second.as_mut_ptr(), second.len() as i32),
                ARES_OK
            );

            assert!(first.iter().any(|value| *value > 0.0));
            assert!(second.iter().any(|value| *value > 0.0));
            assert_ne!(first, second);
            assert!((0.0..=1.0).contains(&ares_get_rms()));
            assert!((0.0..=1.0).contains(&ares_get_peak()));
        });
    }

    #[test]
    fn get_bands_handles_invalid_pointers_and_lengths() {
        with_test_runtime(|| {
            assert_eq!(
                ares_get_bands(ptr::null_mut(), DEFAULT_BAND_COUNT as i32),
                ARES_ERROR_NULL_POINTER
            );

            let mut bands = [0.0_f32; DEFAULT_BAND_COUNT];
            assert_eq!(
                ares_get_bands(bands.as_mut_ptr(), 0),
                ARES_ERROR_INVALID_LENGTH
            );
            assert_eq!(
                ares_get_bands(bands.as_mut_ptr(), -1),
                ARES_ERROR_INVALID_LENGTH
            );

            let mut partial = [0.0_f32; 8];
            assert_eq!(
                ares_get_bands(partial.as_mut_ptr(), partial.len() as i32),
                ARES_STATUS_TRUNCATED
            );
        });
    }

    #[test]
    fn last_error_uses_caller_filled_buffer() {
        with_test_runtime(|| {
            assert_eq!(
                ares_get_bands(ptr::null_mut(), DEFAULT_BAND_COUNT as i32),
                -1
            );

            let mut buffer = [0_i8; 64];
            assert_eq!(
                ares_get_last_error(buffer.as_mut_ptr(), buffer.len()),
                ARES_OK
            );

            let message = unsafe { CStr::from_ptr(buffer.as_ptr()) };
            assert_eq!(message.to_str().unwrap(), "null output pointer");
        });
    }

    #[test]
    fn last_error_handles_invalid_and_short_buffers() {
        with_test_runtime(|| {
            assert_eq!(
                ares_get_bands(ptr::null_mut(), DEFAULT_BAND_COUNT as i32),
                ARES_ERROR_NULL_POINTER
            );

            assert_eq!(
                ares_get_last_error(ptr::null_mut(), 64),
                ARES_ERROR_NULL_POINTER
            );

            let mut empty = [0_i8; 1];
            assert_eq!(
                ares_get_last_error(empty.as_mut_ptr(), 0),
                ARES_ERROR_INVALID_LENGTH
            );

            let mut short = [0_i8; 5];
            assert_eq!(
                ares_get_last_error(short.as_mut_ptr(), short.len()),
                ARES_STATUS_TRUNCATED
            );

            let message = unsafe { CStr::from_ptr(short.as_ptr()) };
            assert_eq!(message.to_str().unwrap(), "null");
        });
    }
}
