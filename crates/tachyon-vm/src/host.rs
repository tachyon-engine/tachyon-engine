//! Host-owned capabilities used by ECMAScript builtins without platform access in engine core.

use core::fmt;

/// Stable provider failure code suitable for Rust and FFI adapters without allocating messages.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostProviderError {
    Unavailable,
    Failure(u32),
}

/// Supplies Unix wall-clock milliseconds for `Date` without exposing `std::time` to the VM.
pub trait WallClockProvider: Send {
    fn unix_time_milliseconds(&mut self) -> Result<i64, HostProviderError>;
}

/// Supplies ECMAScript-compatible local-time conversions without filesystem or locale access.
pub trait TimeZoneProvider: Send {
    /// Returns the local offset applied to one already-UTC epoch millisecond value.
    fn offset_milliseconds_for_utc(
        &mut self,
        utc_milliseconds: i64,
    ) -> Result<i64, HostProviderError>;

    /// Resolves a local wall-time value to UTC using ECMAScript gap/overlap disambiguation.
    fn utc_milliseconds_for_local(
        &mut self,
        local_milliseconds: i64,
    ) -> Result<i64, HostProviderError>;
}

/// Isolate-owned host capabilities; absence remains explicit instead of consulting the process.
#[derive(Default)]
pub struct HostProviders {
    wall_clock: Option<Box<dyn WallClockProvider>>,
    time_zone: Option<Box<dyn TimeZoneProvider>>,
}

impl HostProviders {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            wall_clock: None,
            time_zone: None,
        }
    }

    #[must_use]
    pub fn with_wall_clock(mut self, provider: impl WallClockProvider + 'static) -> Self {
        self.wall_clock = Some(Box::new(provider));
        self
    }

    #[must_use]
    pub fn with_time_zone(mut self, provider: impl TimeZoneProvider + 'static) -> Self {
        self.time_zone = Some(Box::new(provider));
        self
    }

    pub(crate) fn wall_clock_mut(&mut self) -> Option<&mut (dyn WallClockProvider + 'static)> {
        self.wall_clock.as_deref_mut()
    }

    #[allow(
        dead_code,
        reason = "local Date methods consume this provider in the next Date slice"
    )]
    pub(crate) fn time_zone_mut(&mut self) -> Option<&mut (dyn TimeZoneProvider + 'static)> {
        self.time_zone.as_deref_mut()
    }
}

impl fmt::Debug for HostProviders {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HostProviders")
            .field("wall_clock", &self.wall_clock.is_some())
            .field("time_zone", &self.time_zone.is_some())
            .finish()
    }
}
