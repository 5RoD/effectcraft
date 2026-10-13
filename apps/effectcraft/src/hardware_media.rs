//! Register the native decoder before sources open. Failure leaves software decoding available.

pub fn initialize() {
    effectcraft_host::set_hardware_decoding(false);
    match std::env::var("EFFECTCRAFT_HARDWARE_DECODING") {
        Ok(mode) if mode.eq_ignore_ascii_case("off") => {
            log::info!("hardware decoding disabled for this launch");
            return;
        }
        Ok(mode) if mode.eq_ignore_ascii_case("auto") => {}
        Err(std::env::VarError::NotPresent) => {}
        mode => {
            log::warn!("invalid EFFECTCRAFT_HARDWARE_DECODING value {mode:?}; use auto or off; continuing with software decoding");
            return;
        }
    }
    match register_safely(filmcraft_platform::register) {
        Ok(backend) => {
            effectcraft_host::set_hardware_decoding(true);
            log::info!("hardware decoder registered: {backend}; unsupported streams use software decoding");
        }
        Err(reason) => log::warn!("hardware decoder unavailable: {reason}; continuing with software decoding"),
    }
}

fn register_safely(register: impl FnOnce() -> filmcraft_platform::Availability + std::panic::UnwindSafe) -> Result<&'static str, String> {
    match std::panic::catch_unwind(register) {
        Ok(filmcraft_platform::Availability::Available(backend)) => Ok(backend),
        Ok(filmcraft_platform::Availability::Unavailable(reason)) => Err(reason.to_owned()),
        Err(_) => Err("native adapter registration panicked".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaped_adapter_panic_is_an_error() {
        let result = register_safely(|| panic!("synthetic adapter failure"));
        assert_eq!(result, Err("native adapter registration panicked".to_owned()));
    }
}
