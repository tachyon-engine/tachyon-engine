//! Host-owned capabilities used by ECMAScript builtins without platform access in engine core.

use core::{
    fmt,
    num::NonZeroUsize,
    task::{Context, Poll},
    time::Duration,
};

use crate::SharedArrayBufferHandle;

/// Stable provider failure code suitable for Rust and FFI adapters without allocating messages.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostProviderError {
    Unavailable,
    Failure(u32),
}

/// Process-local identity for one shared backing store without exposing its representation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SharedMemoryId(NonZeroUsize);

impl SharedMemoryId {
    pub(crate) fn from_address(address: usize) -> Self {
        Self(NonZeroUsize::new(address).expect("Arc backing addresses are non-zero"))
    }
}

/// Waiter-list key shared by every isolate in one host-defined agent cluster.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct AtomicsWaitLocation {
    memory: SharedMemoryId,
    byte_offset: usize,
}

impl AtomicsWaitLocation {
    pub(crate) const fn new(memory: SharedMemoryId, byte_offset: usize) -> Self {
        Self {
            memory,
            byte_offset,
        }
    }

    #[must_use]
    pub const fn memory(self) -> SharedMemoryId {
        self.memory
    }

    #[must_use]
    pub const fn byte_offset(self) -> usize {
        self.byte_offset
    }
}

/// Stable synchronous result returned by an injected Atomics waiter implementation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AtomicsWaitResult {
    Ok,
    NotEqual,
    TimedOut,
}

/// One host-owned asynchronous wait operation polled only by its originating isolate.
///
/// Implementations may retain synchronization handles and wakers, but must never retain
/// isolate-local `Value` or `GcRef` data. Dropping the operation must unregister any waiter that
/// has not already completed.
pub trait AtomicsAsyncWait: Send {
    fn poll(
        &mut self,
        context: &mut Context<'_>,
    ) -> Poll<Result<AtomicsWaitResult, HostProviderError>>;
}

/// Result of atomically comparing and optionally publishing one asynchronous waiter.
pub enum AtomicsAsyncWaitStart {
    Immediate(AtomicsWaitResult),
    Pending(Box<dyn AtomicsAsyncWait>),
}

/// Engine-neutral scalar accompanying one Test262-style SharedArrayBuffer broadcast.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AgentBroadcastValue {
    Undefined,
    Int32(i32),
    BigInt(Box<[u16]>),
}

/// Owned message transferred from an agent-cluster provider into one isolate.
#[derive(Clone, Debug)]
pub struct AgentBroadcast {
    pub buffer: SharedArrayBufferHandle,
    pub value: AgentBroadcastValue,
}

/// Host-owned waiter registry and parking capability for one ECMAScript agent cluster.
///
/// `wait` must serialize `condition` and waiter publication against `notify` for the same
/// location. The provider owns blocking, clocks, threads, and wakeup primitives; the engine core
/// only supplies a short comparison callback that cannot execute JavaScript or allocate.
pub trait AtomicsWaiterProvider: Send {
    fn notify(
        &mut self,
        location: AtomicsWaitLocation,
        count: u64,
    ) -> Result<u64, HostProviderError>;

    fn wait(
        &mut self,
        location: AtomicsWaitLocation,
        timeout: Option<Duration>,
        condition: &mut dyn FnMut() -> Result<bool, HostProviderError>,
    ) -> Result<AtomicsWaitResult, HostProviderError>;

    /// Compares and publishes without blocking the isolate thread.
    ///
    /// The provider must serialize the supplied condition and waiter publication against
    /// `notify` exactly as for synchronous `wait`. The default keeps existing embedders source
    /// compatible while making missing asynchronous support explicit.
    fn wait_async(
        &mut self,
        _location: AtomicsWaitLocation,
        _timeout: Option<Duration>,
        _condition: &mut dyn FnMut() -> Result<bool, HostProviderError>,
    ) -> Result<AtomicsAsyncWaitStart, HostProviderError> {
        Err(HostProviderError::Unavailable)
    }
}

/// Host-owned lifecycle and coordination capability for one ECMAScript agent cluster.
///
/// The engine transfers only owned UTF-16 strings and opaque SharedArrayBuffer handles across
/// this boundary. Thread creation, blocking, clocks, cancellation, and worker joining remain
/// entirely host-owned.
pub trait AgentHostProvider: Send {
    fn start(&mut self, source: Box<[u16]>) -> Result<(), HostProviderError>;

    fn broadcast(&mut self, message: AgentBroadcast) -> Result<(), HostProviderError>;

    fn receive_broadcast(&mut self) -> Result<AgentBroadcast, HostProviderError>;

    fn report(&mut self, message: Box<[u16]>) -> Result<(), HostProviderError>;

    fn get_report(&mut self) -> Result<Option<Box<[u16]>>, HostProviderError>;

    fn sleep(&mut self, milliseconds: f64) -> Result<(), HostProviderError>;

    fn monotonic_now(&mut self) -> Result<f64, HostProviderError>;

    fn leaving(&mut self) -> Result<(), HostProviderError>;
}

/// Supplies Unix wall-clock milliseconds for `Date` without exposing `std::time` to the VM.
pub trait WallClockProvider: Send {
    fn unix_time_milliseconds(&mut self) -> Result<i64, HostProviderError>;
}

/// Supplies ECMAScript-compatible local-time conversions without filesystem or locale access.
pub trait TimeZoneProvider: Send {
    /// Returns the local offset applied to UTC; values must remain within one civil day.
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

/// Provider-owned data collections exposed by `Intl.supportedValuesOf`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IntlSupportedValuesKey {
    Calendar,
    Collation,
    Currency,
    NumberingSystem,
    TimeZone,
    Unit,
}

/// Supplies locale data operations without allowing the VM to read process or filesystem state.
pub trait IntlProvider: Send {
    /// Returns one canonical BCP 47 locale, or `None` when the input is structurally invalid.
    fn canonicalize_locale(&mut self, locale: &str) -> Result<Option<Box<str>>, HostProviderError>;

    /// Returns the provider's canonical default locale.
    fn default_locale(&mut self) -> Result<Box<str>, HostProviderError>;

    /// Returns the provider's supported values as owned strings with no borrowed ICU backing.
    fn supported_values(
        &mut self,
        key: IntlSupportedValuesKey,
    ) -> Result<Box<[Box<str>]>, HostProviderError>;
}

/// Isolate-owned host capabilities; absence remains explicit instead of consulting the process.
#[derive(Default)]
pub struct HostProviders {
    wall_clock: Option<Box<dyn WallClockProvider>>,
    time_zone: Option<Box<dyn TimeZoneProvider>>,
    intl: Option<Box<dyn IntlProvider>>,
    atomics_waiter: Option<Box<dyn AtomicsWaiterProvider>>,
    agent_host: Option<Box<dyn AgentHostProvider>>,
    agent_can_suspend: bool,
}

impl HostProviders {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            wall_clock: None,
            time_zone: None,
            intl: None,
            atomics_waiter: None,
            agent_host: None,
            agent_can_suspend: false,
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

    #[must_use]
    pub fn with_intl(mut self, provider: impl IntlProvider + 'static) -> Self {
        self.intl = Some(Box::new(provider));
        self
    }

    #[must_use]
    pub fn with_atomics_waiter(mut self, provider: impl AtomicsWaiterProvider + 'static) -> Self {
        self.atomics_waiter = Some(Box::new(provider));
        self
    }

    #[must_use]
    pub fn with_agent_host(mut self, provider: impl AgentHostProvider + 'static) -> Self {
        self.agent_host = Some(Box::new(provider));
        self
    }

    #[must_use]
    pub const fn with_agent_can_suspend(mut self, can_suspend: bool) -> Self {
        self.agent_can_suspend = can_suspend;
        self
    }

    pub(crate) fn wall_clock_mut(&mut self) -> Option<&mut (dyn WallClockProvider + 'static)> {
        self.wall_clock.as_deref_mut()
    }

    pub(crate) fn time_zone_mut(&mut self) -> Option<&mut (dyn TimeZoneProvider + 'static)> {
        self.time_zone.as_deref_mut()
    }

    pub(crate) fn intl_mut(&mut self) -> Option<&mut (dyn IntlProvider + 'static)> {
        self.intl.as_deref_mut()
    }

    pub(crate) fn atomics_waiter_mut(
        &mut self,
    ) -> Option<&mut (dyn AtomicsWaiterProvider + 'static)> {
        self.atomics_waiter.as_deref_mut()
    }

    pub(crate) fn agent_host_mut(&mut self) -> Option<&mut (dyn AgentHostProvider + 'static)> {
        self.agent_host.as_deref_mut()
    }

    pub(crate) const fn agent_can_suspend(&self) -> bool {
        self.agent_can_suspend
    }
}

impl fmt::Debug for HostProviders {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HostProviders")
            .field("wall_clock", &self.wall_clock.is_some())
            .field("time_zone", &self.time_zone.is_some())
            .field("intl", &self.intl.is_some())
            .field("atomics_waiter", &self.atomics_waiter.is_some())
            .field("agent_host", &self.agent_host.is_some())
            .field("agent_can_suspend", &self.agent_can_suspend)
            .finish()
    }
}
