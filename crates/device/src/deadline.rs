//! The time a piece of device work may take, shared by all its steps.
use orchard_cache::{Code, Error, Result};
use std::time::{Duration, Instant};

/// A point in time by which the work must be done.
#[derive(Debug, Clone, Copy)]
pub struct Deadline(Instant);

impl Deadline {
    pub fn after(timeout: Duration) -> Deadline {
        Deadline(Instant::now() + timeout)
    }

    /// The time left, or a timeout error when there is none.
    pub fn left(&self) -> Result<Duration> {
        let left = self.0.saturating_duration_since(Instant::now());
        // USB timeouts count milliseconds, and zero would mean no timeout.
        if left < Duration::from_millis(1) {
            return Err(Error::new(Code::Timeout, "timed out"));
        }
        Ok(left)
    }

    /// The timeout for one step: `most`, or the time left when that is less.
    pub fn step(&self, most: Duration) -> Result<Duration> {
        Ok(self.left()?.min(most))
    }

    /// Sleeps for `most`, or until the deadline when that comes first.
    pub fn sleep(&self, most: Duration) -> Result<()> {
        std::thread::sleep(self.step(most)?);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_step_gets_the_smaller_of_its_own_time_and_the_time_left() {
        let deadline = Deadline::after(Duration::from_secs(60));
        assert_eq!(
            deadline.step(Duration::from_secs(5)).unwrap(),
            Duration::from_secs(5)
        );
        assert!(deadline.step(Duration::from_secs(600)).unwrap() <= Duration::from_secs(60));
    }

    #[test]
    fn no_time_left_is_a_timeout() {
        let deadline = Deadline::after(Duration::ZERO);
        assert_eq!(deadline.left().unwrap_err().code, Code::Timeout);
        assert_eq!(
            deadline.sleep(Duration::from_secs(1)).unwrap_err().code,
            Code::Timeout
        );
    }
}
