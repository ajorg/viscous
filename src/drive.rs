//! A shared primitive for VISCA controls that only expose continuous
//! start/stop drive commands, with no relative-move equivalent (zoom and
//! focus, unlike pan/tilt).

use std::time::Duration;

use grafton_visca::Error;

/// Runs a continuous VISCA drive command for `duration`, then stops it.
///
/// If `start` fails, `stop` is not called, since the drive was never
/// actually started.
pub fn timed_drive(
    start: impl FnOnce() -> Result<(), Error>,
    stop: impl FnOnce() -> Result<(), Error>,
    duration: Duration,
) -> Result<(), Error> {
    start()?;
    std::thread::sleep(duration);
    stop()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn timed_drive_calls_start_then_stop() {
        let started = Cell::new(false);
        let stopped = Cell::new(false);

        let result = timed_drive(
            || {
                started.set(true);
                Ok(())
            },
            || {
                assert!(started.get(), "stop should be called after start");
                stopped.set(true);
                Ok(())
            },
            Duration::ZERO,
        );

        assert!(result.is_ok());
        assert!(started.get());
        assert!(stopped.get());
    }

    #[test]
    fn timed_drive_skips_stop_when_start_fails() {
        let stopped = Cell::new(false);

        let result = timed_drive(
            || Err(Error::Timeout),
            || {
                stopped.set(true);
                Ok(())
            },
            Duration::ZERO,
        );

        assert!(result.is_err());
        assert!(!stopped.get(), "stop should not be called when start fails");
    }
}
