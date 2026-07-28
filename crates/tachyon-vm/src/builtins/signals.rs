//! Native TC39 Signals graph payloads and the first executable API slice.

use super::super::*;
use tachyon_gc::WeakGcRef;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ComputedState {
    Clean,
    Checked,
    Computing,
    Dirty,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum WatcherState {
    Waiting,
    Watching,
    Pending,
}

#[derive(Debug)]
pub(crate) struct OrderedSignals {
    entries: Vec<Value>,
}

impl OrderedSignals {
    fn try_new() -> Result<Self, ExecutionError> {
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(tuning::signals::INITIAL_EDGE_CAPACITY)
            .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
        Ok(Self { entries })
    }

    fn insert(&mut self, value: Value) -> Result<bool, ExecutionError> {
        if self.entries.contains(&value) {
            return Ok(false);
        }
        if self.entries.len() == self.entries.capacity() {
            self.entries
                .try_reserve_exact(1)
                .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
        }
        self.entries.push(value);
        Ok(true)
    }

    fn remove(&mut self, value: Value) -> bool {
        let Some(index) = self.entries.iter().position(|entry| *entry == value) else {
            return false;
        };
        self.entries.remove(index);
        true
    }

    fn try_snapshot(&self) -> Result<Vec<Value>, ExecutionError> {
        let mut snapshot = Vec::new();
        snapshot
            .try_reserve_exact(self.entries.len())
            .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
        snapshot.extend_from_slice(&self.entries);
        Ok(snapshot)
    }
}

impl Trace for OrderedSignals {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.entries.trace(tracer);
    }
}

#[derive(Clone, Copy, Debug)]
struct SignalSinkEdge {
    weak: WeakGcRef<()>,
    live: Option<Value>,
}

impl SignalSinkEdge {
    #[inline(always)]
    fn value(self) -> Option<Value> {
        self.live.or_else(|| {
            self.weak
                .get()
                .map(|reference| Value::from_heap_ref(reference.raw()))
        })
    }
}

impl Trace for SignalSinkEdge {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.live.trace(tracer);
        self.weak.trace(tracer);
    }
}

/// Ordered reverse edges whose cold identities do not keep dependents alive.
#[derive(Debug)]
struct OrderedSignalSinks {
    entries: Vec<SignalSinkEdge>,
}

impl OrderedSignalSinks {
    fn try_new() -> Result<Self, ExecutionError> {
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(tuning::signals::INITIAL_EDGE_CAPACITY)
            .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
        Ok(Self { entries })
    }

    /// Reuses cleared weak slots and promotes an existing edge without changing insertion order.
    fn insert(&mut self, value: Value, live: bool) -> Result<bool, ExecutionError> {
        if let Some(edge) = self
            .entries
            .iter_mut()
            .find(|edge| edge.value() == Some(value))
        {
            if live {
                edge.live = Some(value);
            }
            return Ok(false);
        }
        let reference = value
            .as_heap_ref()
            .ok_or(ExecutionError::NotObject(value))?;
        self.entries.retain(|edge| edge.value().is_some());
        if self.entries.len() == self.entries.capacity() {
            self.entries
                .try_reserve_exact(1)
                .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
        }
        self.entries.push(SignalSinkEdge {
            weak: WeakGcRef::new(GcRef::from_erased_raw(reference)),
            live: live.then_some(value),
        });
        Ok(true)
    }

    fn remove(&mut self, value: Value) -> bool {
        let Some(index) = self
            .entries
            .iter()
            .position(|edge| edge.value() == Some(value))
        else {
            return false;
        };
        self.entries.remove(index);
        true
    }

    fn set_live(&mut self, value: Value, live: bool) -> Result<(), ExecutionError> {
        let edge = self
            .entries
            .iter_mut()
            .find(|edge| edge.value() == Some(value))
            .ok_or(ExecutionError::NotObject(value))?;
        edge.live = live.then_some(value);
        Ok(())
    }

    fn try_snapshot(&self) -> Result<Vec<Value>, ExecutionError> {
        let mut snapshot = Vec::new();
        snapshot
            .try_reserve_exact(self.entries.len())
            .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
        snapshot.extend(self.entries.iter().filter_map(|edge| edge.value()));
        Ok(snapshot)
    }
}

impl Trace for OrderedSignalSinks {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.entries.trace(tracer);
    }
}

#[derive(Debug)]
#[repr(C)]
pub(crate) struct StateSignal {
    value: Value,
    equals: Value,
    watched: Value,
    unwatched: Value,
    sinks: OrderedSignalSinks,
    live_sinks: u32,
    pub(crate) ordinary: OrdinaryObject,
}

impl Trace for StateSignal {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.value.trace(tracer);
        self.equals.trace(tracer);
        self.watched.trace(tracer);
        self.unwatched.trace(tracer);
        self.sinks.trace(tracer);
        self.ordinary.trace(tracer);
    }
}

#[derive(Debug)]
#[repr(C)]
pub(crate) struct ComputedSignal {
    callback: Value,
    cached: Value,
    state: ComputedState,
    sources: OrderedSignals,
    sinks: OrderedSignalSinks,
    live_sinks: u32,
    generation: u64,
    pub(crate) ordinary: OrdinaryObject,
}

const COMPUTED_UNINITIALIZED_GENERATION: u64 = u64::MAX;
const COMPUTED_THROW_COMPLETION_BIT: u64 = 1 << 63;
const COMPUTED_GENERATION_MASK: u64 = !COMPUTED_THROW_COMPLETION_BIT;
const COMPUTED_CALLBACK_SLOT: usize = 0;
const COMPUTED_EQUALS_SLOT: usize = 1;
const COMPUTED_WATCHED_SLOT: usize = 2;
const COMPUTED_UNWATCHED_SLOT: usize = 3;
const COMPUTED_OPTIONS_EQUALS_SLOT: usize = 2;
const COMPUTED_OPTIONS_WATCHED_SLOT: usize = 3;
const COMPUTED_OPTIONS_UNWATCHED_SLOT: usize = 4;
const COMPUTED_OPTIONS_RECORD_COUNT: u8 = 5;
const SIGNAL_OPTIONS_WATCHED_KEY_SLOT: usize = 3;
const SIGNAL_OPTIONS_UNWATCHED_KEY_SLOT: usize = 4;

#[derive(Clone, Copy, Debug)]
struct SignalComputedCallbacks {
    callback: Value,
    equals: Value,
    watched: Value,
    unwatched: Value,
}

#[inline(always)]
const fn computed_generation_is_throw(generation: u64) -> bool {
    generation != COMPUTED_UNINITIALIZED_GENERATION
        && generation & COMPUTED_THROW_COMPLETION_BIT != 0
}

#[inline(always)]
fn signal_previous_computing(value: Value) -> Option<Value> {
    (value.as_immediate() != Some(Immediate::Undefined)).then_some(value)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
enum SignalWatcherOperationKind {
    Watch,
    Unwatch,
    ComputedPull,
    ComputedEquals,
    Notify,
}

#[derive(Clone, Copy, Debug)]
struct SignalComputedPullFrame {
    computed: Value,
    next_source: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
enum SignalLifecycleHookKind {
    Watched,
    Unwatched,
    Pull,
}

#[derive(Clone, Copy, Debug)]
struct SignalLifecycleHook {
    signal: Value,
    kind: SignalLifecycleHookKind,
    next_source: u32,
}

impl Trace for SignalLifecycleHook {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.signal.trace(tracer);
    }
}

/// Traced arguments and hook queue for one resumable Watcher mutation.
#[derive(Debug)]
pub(crate) struct PendingSignalWatcherOperation {
    watcher: Value,
    arguments: Vec<Value>,
    hooks: Vec<SignalLifecycleHook>,
    argument_index: usize,
    hook_index: usize,
    kind: SignalWatcherOperationKind,
}

#[derive(Debug)]
struct SignalWatcherOperationSnapshot {
    watcher: Value,
    argument: Option<Value>,
    hook: Option<SignalLifecycleHook>,
    kind: SignalWatcherOperationKind,
}

impl Trace for PendingSignalWatcherOperation {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.watcher.trace(tracer);
        self.arguments.trace(tracer);
        self.hooks.trace(tracer);
    }
}

struct SignalWatcherAllocationRoots<'a> {
    vm: VmRoots<'a>,
    watcher: Value,
    arguments: Vec<Value>,
}

impl Trace for SignalWatcherAllocationRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.watcher.trace(tracer);
        self.arguments.trace(tracer);
    }
}

struct SignalStateRoots<'a> {
    vm: VmRoots<'a>,
    pending: NativeCallState,
}

impl Trace for SignalStateRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.pending.trace(tracer);
    }
}

struct SignalIntrospectionRoots<'a> {
    vm: VmRoots<'a>,
    prototype: Value,
    entries: Vec<Value>,
    storage: Option<GcRef<PropertyStorage>>,
    elements: Option<GcRef<ArrayElements>>,
}

impl Trace for SignalIntrospectionRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.prototype.trace(tracer);
        self.entries.trace(tracer);
        self.storage.trace(tracer);
        self.elements.trace(tracer);
    }
}

impl Trace for ComputedSignal {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.callback.trace(tracer);
        self.cached.trace(tracer);
        self.sources.trace(tracer);
        self.sinks.trace(tracer);
        self.ordinary.trace(tracer);
    }
}

#[derive(Debug)]
#[repr(C)]
pub(crate) struct WatcherSignal {
    notify: Value,
    state: WatcherState,
    watched: OrderedSignals,
    pending: OrderedSignals,
    pub(crate) ordinary: OrdinaryObject,
}

impl Trace for WatcherSignal {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.notify.trace(tracer);
        self.watched.trace(tracer);
        self.pending.trace(tracer);
        self.ordinary.trace(tracer);
    }
}

#[derive(Debug, Default)]
pub(crate) struct SignalRuntime {
    pub(crate) computing: Option<Value>,
    pub(crate) frozen: bool,
    pub(crate) generation: u64,
    worklist: Vec<Value>,
}

mod computed;
mod graph;
mod runtime;
mod state;
mod watcher;
