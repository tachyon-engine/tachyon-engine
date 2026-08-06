//! Runner-owned Test262 agent cluster, parking registry, and worker lifecycle.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{Arc, Condvar, Mutex, mpsc},
    task::{Context, Poll, Waker},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use tachyon_vm::{
    AgentBroadcast, AgentHostProvider, AtomicsAsyncWait, AtomicsAsyncWaitStart,
    AtomicsWaitLocation, AtomicsWaitResult, AtomicsWaiterProvider, HostProviderError,
};

use crate::tachyon::run_agent_worker;

const MAX_AGENT_SLEEP: Duration = Duration::from_secs(60);
const MAX_AGENT_WORKERS: u64 = 64;
const MAX_AGENT_REPORTS: usize = 4_096;
const INITIAL_AGENT_WORKER_CAPACITY: usize = 8;
const INITIAL_AGENT_REPORT_CAPACITY: usize = 16;
const INITIAL_AGENT_LOCATION_CAPACITY: usize = 8;
const INITIAL_ASYNC_DEADLINE_CAPACITY: usize = 4;

/// Main-request owner that guarantees cancellation and joining on every return path.
pub(super) struct AgentController {
    cluster: Arc<Test262AgentCluster>,
}

impl AgentController {
    pub(super) fn new() -> Self {
        Self {
            cluster: Arc::new(Test262AgentCluster::new()),
        }
    }

    pub(super) fn main_host(&self) -> Test262AgentHost {
        Test262AgentHost::main(Arc::clone(&self.cluster))
    }

    pub(super) fn waiter(&self) -> Test262AtomicsWaiter {
        Test262AtomicsWaiter {
            cluster: Arc::clone(&self.cluster),
        }
    }
}

impl Drop for AgentController {
    fn drop(&mut self) {
        self.cluster.cancel_and_join();
    }
}

struct Broadcast {
    message: AgentBroadcast,
    recipients: HashSet<u64>,
}

struct WaiterRegistration {
    id: u64,
    completion: Option<Arc<AsyncWaitCompletion>>,
}

#[derive(Default)]
struct AsyncWaitCompletion {
    state: Mutex<AsyncWaitCompletionState>,
}

#[derive(Default)]
struct AsyncWaitCompletionState {
    outcome: Option<Result<AtomicsWaitResult, HostProviderError>>,
    waker: Option<Waker>,
}

impl AsyncWaitCompletion {
    /// Publishes exactly one terminal result and wakes the isolate executor outside cluster locks.
    fn complete(&self, outcome: Result<AtomicsWaitResult, HostProviderError>) {
        let waker = self.state.lock().ok().and_then(|mut state| {
            if state.outcome.is_some() {
                return None;
            }
            state.outcome = Some(outcome);
            state.waker.take()
        });
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

struct Test262AgentState {
    waiter_queues: HashMap<AtomicsWaitLocation, VecDeque<WaiterRegistration>>,
    async_deadlines: HashMap<u64, (Instant, AtomicsWaitLocation)>,
    next_waiter: u64,
    next_worker: u64,
    ready_workers: HashSet<u64>,
    broadcast: Option<Broadcast>,
    reports: VecDeque<Box<[u16]>>,
    cancelled: bool,
}

impl Test262AgentState {
    fn new() -> Self {
        Self {
            waiter_queues: HashMap::with_capacity(INITIAL_AGENT_LOCATION_CAPACITY),
            async_deadlines: HashMap::with_capacity(INITIAL_ASYNC_DEADLINE_CAPACITY),
            next_waiter: 0,
            next_worker: 0,
            ready_workers: HashSet::with_capacity(INITIAL_AGENT_WORKER_CAPACITY),
            broadcast: None,
            reports: VecDeque::with_capacity(INITIAL_AGENT_REPORT_CAPACITY),
            cancelled: false,
        }
    }
}

/// One synchronization owner shared by agent messaging and Atomics waiter lists.
pub(super) struct Test262AgentCluster {
    state: Mutex<Test262AgentState>,
    changed: Condvar,
    workers: Mutex<Vec<JoinHandle<()>>>,
    timer_worker: Mutex<Option<JoinHandle<()>>>,
    started: Instant,
}

impl Test262AgentCluster {
    fn new() -> Self {
        Self {
            state: Mutex::new(Test262AgentState::new()),
            changed: Condvar::new(),
            workers: Mutex::new(Vec::with_capacity(INITIAL_AGENT_WORKER_CAPACITY)),
            timer_worker: Mutex::new(None),
            started: Instant::now(),
        }
    }

    fn allocate_worker(&self) -> Result<u64, HostProviderError> {
        let mut state = self.state.lock().map_err(provider_failure)?;
        if state.cancelled {
            return Err(HostProviderError::Unavailable);
        }
        if state.next_worker >= MAX_AGENT_WORKERS {
            return Err(HostProviderError::Failure(7));
        }
        let worker = state.next_worker;
        state.next_worker = state.next_worker.wrapping_add(1);
        Ok(worker)
    }

    pub(super) fn worker_ready(&self, worker: u64) -> Result<(), HostProviderError> {
        let mut state = self.state.lock().map_err(provider_failure)?;
        if state.cancelled {
            return Err(HostProviderError::Unavailable);
        }
        state.ready_workers.insert(worker);
        self.changed.notify_all();
        Ok(())
    }

    pub(super) fn worker_host(self: &Arc<Self>, worker: u64) -> Test262AgentHost {
        Test262AgentHost::worker(Arc::clone(self), worker)
    }

    pub(super) fn waiter(self: &Arc<Self>) -> Test262AtomicsWaiter {
        Test262AtomicsWaiter {
            cluster: Arc::clone(self),
        }
    }

    /// Removes a worker from every rendezvous so failures cannot strand a broadcaster.
    pub(super) fn worker_finished(&self, worker: u64) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        state.ready_workers.remove(&worker);
        if let Some(broadcast) = &mut state.broadcast {
            broadcast.recipients.remove(&worker);
            if broadcast.recipients.is_empty() {
                state.broadcast = None;
            }
        }
        self.changed.notify_all();
    }

    fn push_worker(&self, handle: JoinHandle<()>) -> Result<(), JoinHandle<()>> {
        match self.workers.lock() {
            Ok(mut workers) => {
                workers.push(handle);
                Ok(())
            }
            Err(_) => Err(handle),
        }
    }

    /// Starts one request-scoped timer coordinator only when the first finite async wait appears.
    fn ensure_timer_worker(self: &Arc<Self>) -> Result<(), HostProviderError> {
        let mut timer = self.timer_worker.lock().map_err(provider_failure)?;
        if timer.is_some() {
            return Ok(());
        }
        let cluster = Arc::clone(self);
        let handle = thread::Builder::new()
            .name("test262-atomics-timer".into())
            .spawn(move || cluster.run_timer_worker())
            .map_err(|_| HostProviderError::Failure(10))?;
        *timer = Some(handle);
        Ok(())
    }

    /// Waits for the nearest deadline, removing exactly the registration that wins the race.
    fn run_timer_worker(self: Arc<Self>) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        loop {
            if state.cancelled {
                return;
            }
            let next = state
                .async_deadlines
                .iter()
                .min_by_key(|(_, (deadline, _))| *deadline)
                .map(|(waiter, (deadline, location))| (*waiter, *deadline, *location));
            let Some((waiter, deadline, location)) = next else {
                let Ok(next) = self.changed.wait(state) else {
                    return;
                };
                state = next;
                continue;
            };
            let now = Instant::now();
            if let Some(remaining) = deadline.checked_duration_since(now) {
                let Ok((next, _)) = self.changed.wait_timeout(state, remaining) else {
                    return;
                };
                state = next;
                continue;
            }
            state.async_deadlines.remove(&waiter);
            let completion = remove_waiter(&mut state, location, waiter)
                .and_then(|registration| registration.completion);
            drop(state);
            if let Some(completion) = completion {
                completion.complete(Ok(AtomicsWaitResult::TimedOut));
            }
            self.changed.notify_all();
            let Ok(next) = self.state.lock() else {
                return;
            };
            state = next;
        }
    }

    fn cancel(&self) {
        let completions = if let Ok(mut state) = self.state.lock() {
            state.cancelled = true;
            state.async_deadlines.clear();
            let completions = state
                .waiter_queues
                .drain()
                .flat_map(|(_, queue)| queue)
                .filter_map(|registration| registration.completion)
                .collect::<Vec<_>>();
            state.broadcast = None;
            completions
        } else {
            Vec::new()
        };
        for completion in completions {
            completion.complete(Err(HostProviderError::Unavailable));
        }
        self.changed.notify_all();
    }

    /// Wakes every blocking host operation, then joins workers without holding cluster locks.
    fn cancel_and_join(&self) {
        self.cancel();
        let handles = self
            .workers
            .lock()
            .map(|mut workers| workers.drain(..).collect::<Vec<_>>())
            .unwrap_or_default();
        for handle in handles {
            let _ = handle.join();
        }
        if let Ok(mut timer) = self.timer_worker.lock()
            && let Some(handle) = timer.take()
        {
            let _ = handle.join();
        }
    }

    /// Removes a dropped async handle from both the waiter FIFO and deadline registry.
    fn cancel_async_wait(&self, location: AtomicsWaitLocation, waiter: u64) {
        if let Ok(mut state) = self.state.lock() {
            state.async_deadlines.remove(&waiter);
            let _ = remove_waiter(&mut state, location, waiter);
        }
        self.changed.notify_all();
    }
}

#[derive(Clone)]
pub(super) struct Test262AtomicsWaiter {
    cluster: Arc<Test262AgentCluster>,
}

impl AtomicsWaiterProvider for Test262AtomicsWaiter {
    /// Removes the requested FIFO prefix while holding the shared location critical section.
    fn notify(
        &mut self,
        location: AtomicsWaitLocation,
        count: u64,
    ) -> Result<u64, HostProviderError> {
        let mut state = self.cluster.state.lock().map_err(provider_failure)?;
        let mut notified = 0_u64;
        let mut removed = Vec::new();
        if let Some(queue) = state.waiter_queues.get_mut(&location) {
            while notified < count {
                let Some(registration) = queue.pop_front() else {
                    break;
                };
                removed.push(registration);
                notified += 1;
            }
            if queue.is_empty() {
                state.waiter_queues.remove(&location);
            }
        }
        for registration in &removed {
            state.async_deadlines.remove(&registration.id);
        }
        drop(state);
        for registration in removed {
            if let Some(completion) = registration.completion {
                completion.complete(Ok(AtomicsWaitResult::Ok));
            }
        }
        if notified != 0 {
            self.cluster.changed.notify_all();
        }
        Ok(notified)
    }

    /// Compares and publishes atomically against notify, then parks only inside the runner.
    fn wait(
        &mut self,
        location: AtomicsWaitLocation,
        timeout: Option<Duration>,
        condition: &mut dyn FnMut() -> Result<bool, HostProviderError>,
    ) -> Result<AtomicsWaitResult, HostProviderError> {
        let mut state = self.cluster.state.lock().map_err(provider_failure)?;
        if state.cancelled {
            return Err(HostProviderError::Unavailable);
        }
        if !condition()? {
            return Ok(AtomicsWaitResult::NotEqual);
        }
        if timeout == Some(Duration::ZERO) {
            return Ok(AtomicsWaitResult::TimedOut);
        }
        let waiter = state.next_waiter;
        state.next_waiter = state
            .next_waiter
            .checked_add(1)
            .ok_or(HostProviderError::Failure(11))?;
        state
            .waiter_queues
            .entry(location)
            .or_default()
            .push_back(WaiterRegistration {
                id: waiter,
                completion: None,
            });
        self.wait_loop(state, location, waiter, timeout)
    }

    /// Publishes an engine-neutral handle while preserving compare/enqueue atomicity.
    fn wait_async(
        &mut self,
        location: AtomicsWaitLocation,
        timeout: Option<Duration>,
        condition: &mut dyn FnMut() -> Result<bool, HostProviderError>,
    ) -> Result<AtomicsAsyncWaitStart, HostProviderError> {
        if timeout.is_some_and(|timeout| !timeout.is_zero()) {
            self.cluster.ensure_timer_worker()?;
        }
        let mut state = self.cluster.state.lock().map_err(provider_failure)?;
        if state.cancelled {
            return Err(HostProviderError::Unavailable);
        }
        if !condition()? {
            return Ok(AtomicsAsyncWaitStart::Immediate(
                AtomicsWaitResult::NotEqual,
            ));
        }
        if timeout == Some(Duration::ZERO) {
            return Ok(AtomicsAsyncWaitStart::Immediate(
                AtomicsWaitResult::TimedOut,
            ));
        }
        let waiter = state.next_waiter;
        state.next_waiter = state
            .next_waiter
            .checked_add(1)
            .ok_or(HostProviderError::Failure(11))?;
        let completion = Arc::new(AsyncWaitCompletion::default());
        let deadline = timeout
            .map(|timeout| {
                Instant::now()
                    .checked_add(timeout)
                    .ok_or(HostProviderError::Failure(12))
            })
            .transpose()?;
        state
            .waiter_queues
            .entry(location)
            .or_default()
            .push_back(WaiterRegistration {
                id: waiter,
                completion: Some(Arc::clone(&completion)),
            });
        if let Some(deadline) = deadline {
            state.async_deadlines.insert(waiter, (deadline, location));
        }
        drop(state);
        self.cluster.changed.notify_all();
        Ok(AtomicsAsyncWaitStart::Pending(Box::new(Test262AsyncWait {
            cluster: Arc::clone(&self.cluster),
            location,
            waiter,
            completion,
        })))
    }
}

struct Test262AsyncWait {
    cluster: Arc<Test262AgentCluster>,
    location: AtomicsWaitLocation,
    waiter: u64,
    completion: Arc<AsyncWaitCompletion>,
}

impl AtomicsAsyncWait for Test262AsyncWait {
    fn poll(
        &mut self,
        context: &mut Context<'_>,
    ) -> Poll<Result<AtomicsWaitResult, HostProviderError>> {
        let Ok(mut state) = self.completion.state.lock() else {
            return Poll::Ready(Err(HostProviderError::Failure(1)));
        };
        if let Some(outcome) = state.outcome.take() {
            return Poll::Ready(outcome);
        }
        let replace = state
            .waker
            .as_ref()
            .is_none_or(|registered| !registered.will_wake(context.waker()));
        if replace {
            state.waker = Some(context.waker().clone());
        }
        Poll::Pending
    }
}

impl Drop for Test262AsyncWait {
    fn drop(&mut self) {
        self.cluster.cancel_async_wait(self.location, self.waiter);
    }
}

impl Test262AtomicsWaiter {
    /// Parks until notify, timeout, or request cancellation removes this waiter registration.
    fn wait_loop<'a>(
        &self,
        mut state: std::sync::MutexGuard<'a, Test262AgentState>,
        location: AtomicsWaitLocation,
        waiter: u64,
        timeout: Option<Duration>,
    ) -> Result<AtomicsWaitResult, HostProviderError> {
        let started = Instant::now();
        loop {
            if state.cancelled {
                let _ = remove_waiter(&mut state, location, waiter);
                return Err(HostProviderError::Unavailable);
            }
            if !waiter_is_registered(&state, location, waiter) {
                return Ok(AtomicsWaitResult::Ok);
            }
            let Some(limit) = timeout else {
                state = self.cluster.changed.wait(state).map_err(provider_failure)?;
                continue;
            };
            let Some(remaining) = limit.checked_sub(started.elapsed()) else {
                let _ = remove_waiter(&mut state, location, waiter);
                return Ok(AtomicsWaitResult::TimedOut);
            };
            let (next, elapsed) = self
                .cluster
                .changed
                .wait_timeout(state, remaining)
                .map_err(provider_failure)?;
            state = next;
            if elapsed.timed_out() && waiter_is_registered(&state, location, waiter) {
                let _ = remove_waiter(&mut state, location, waiter);
                return Ok(AtomicsWaitResult::TimedOut);
            }
        }
    }
}

enum AgentRole {
    Main,
    Worker(Option<u64>),
}

pub(super) struct Test262AgentHost {
    cluster: Arc<Test262AgentCluster>,
    role: AgentRole,
}

impl Test262AgentHost {
    fn main(cluster: Arc<Test262AgentCluster>) -> Self {
        Self {
            cluster,
            role: AgentRole::Main,
        }
    }

    fn worker(cluster: Arc<Test262AgentCluster>, worker: u64) -> Self {
        Self {
            cluster,
            role: AgentRole::Worker(Some(worker)),
        }
    }

    fn worker_id(&self) -> Result<u64, HostProviderError> {
        match self.role {
            AgentRole::Worker(Some(worker)) => Ok(worker),
            AgentRole::Main | AgentRole::Worker(None) => Err(HostProviderError::Unavailable),
        }
    }
}

impl AgentHostProvider for Test262AgentHost {
    /// Spawns one isolated worker and blocks until its hooks and source have compiled.
    fn start(&mut self, source: Box<[u16]>) -> Result<(), HostProviderError> {
        if !matches!(self.role, AgentRole::Main) {
            return Err(HostProviderError::Unavailable);
        }
        let worker = self.cluster.allocate_worker()?;
        let cluster = Arc::clone(&self.cluster);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let handle = thread::Builder::new()
            .name(format!("test262-agent-{worker}"))
            .spawn(move || run_agent_worker(source, cluster, worker, ready_tx))
            .map_err(|_| HostProviderError::Failure(2))?;
        if let Err(handle) = self.cluster.push_worker(handle) {
            self.cluster.cancel();
            let _ = handle.join();
            return Err(HostProviderError::Failure(8));
        }
        ready_rx.recv().map_err(|_| HostProviderError::Failure(3))?
    }

    /// Publishes one SAB to the current ready-worker set and waits for every recipient.
    fn broadcast(&mut self, message: AgentBroadcast) -> Result<(), HostProviderError> {
        if !matches!(self.role, AgentRole::Main) {
            return Err(HostProviderError::Unavailable);
        }
        let mut state = self.cluster.state.lock().map_err(provider_failure)?;
        while state.broadcast.is_some() && !state.cancelled {
            state = self.cluster.changed.wait(state).map_err(provider_failure)?;
        }
        if state.cancelled {
            return Err(HostProviderError::Unavailable);
        }
        let recipients = state.ready_workers.clone();
        if recipients.is_empty() {
            return Ok(());
        }
        state.broadcast = Some(Broadcast {
            message,
            recipients,
        });
        self.cluster.changed.notify_all();
        while state.broadcast.is_some() && !state.cancelled {
            state = self.cluster.changed.wait(state).map_err(provider_failure)?;
        }
        if state.cancelled {
            Err(HostProviderError::Unavailable)
        } else {
            Ok(())
        }
    }

    /// Waits for a broadcast addressed to this worker and acknowledges retrieval atomically.
    fn receive_broadcast(&mut self) -> Result<AgentBroadcast, HostProviderError> {
        let worker = self.worker_id()?;
        let mut state = self.cluster.state.lock().map_err(provider_failure)?;
        loop {
            if state.cancelled {
                return Err(HostProviderError::Unavailable);
            }
            let addressed = state
                .broadcast
                .as_ref()
                .is_some_and(|broadcast| broadcast.recipients.contains(&worker));
            if addressed {
                let broadcast = state
                    .broadcast
                    .as_mut()
                    .expect("addressed worker observes a live broadcast");
                let message = broadcast.message.clone();
                broadcast.recipients.remove(&worker);
                if broadcast.recipients.is_empty() {
                    state.broadcast = None;
                }
                self.cluster.changed.notify_all();
                return Ok(message);
            }
            state = self.cluster.changed.wait(state).map_err(provider_failure)?;
        }
    }

    fn report(&mut self, message: Box<[u16]>) -> Result<(), HostProviderError> {
        self.worker_id()?;
        let mut state = self.cluster.state.lock().map_err(provider_failure)?;
        if state.cancelled {
            return Err(HostProviderError::Unavailable);
        }
        if state.reports.len() >= MAX_AGENT_REPORTS {
            return Err(HostProviderError::Failure(9));
        }
        state.reports.push_back(message);
        self.cluster.changed.notify_all();
        Ok(())
    }

    fn get_report(&mut self) -> Result<Option<Box<[u16]>>, HostProviderError> {
        if !matches!(self.role, AgentRole::Main) {
            return Err(HostProviderError::Unavailable);
        }
        let mut state = self.cluster.state.lock().map_err(provider_failure)?;
        Ok(state.reports.pop_front())
    }

    fn sleep(&mut self, milliseconds: f64) -> Result<(), HostProviderError> {
        if self
            .cluster
            .state
            .lock()
            .map_err(provider_failure)?
            .cancelled
        {
            return Err(HostProviderError::Unavailable);
        }
        let seconds = if milliseconds.is_nan() || milliseconds <= 0.0 {
            0.0
        } else if milliseconds.is_infinite() {
            MAX_AGENT_SLEEP.as_secs_f64()
        } else {
            (milliseconds / 1_000.0).min(MAX_AGENT_SLEEP.as_secs_f64())
        };
        thread::sleep(Duration::from_secs_f64(seconds));
        Ok(())
    }

    fn monotonic_now(&mut self) -> Result<f64, HostProviderError> {
        Ok(self.cluster.started.elapsed().as_secs_f64() * 1_000.0)
    }

    fn leaving(&mut self) -> Result<(), HostProviderError> {
        let AgentRole::Worker(worker) = &mut self.role else {
            return Err(HostProviderError::Unavailable);
        };
        if let Some(worker) = worker.take() {
            self.cluster.worker_finished(worker);
        }
        Ok(())
    }
}

impl Drop for Test262AgentHost {
    fn drop(&mut self) {
        if let AgentRole::Worker(Some(worker)) = self.role {
            self.cluster.worker_finished(worker);
        }
    }
}

#[inline]
fn provider_failure<T>(_error: std::sync::PoisonError<T>) -> HostProviderError {
    HostProviderError::Failure(1)
}

#[inline]
fn waiter_is_registered(
    state: &Test262AgentState,
    location: AtomicsWaitLocation,
    waiter: u64,
) -> bool {
    state
        .waiter_queues
        .get(&location)
        .is_some_and(|queue| queue.iter().any(|registration| registration.id == waiter))
}

/// Removes one timed-out waiter without disturbing FIFO order for remaining agents.
fn remove_waiter(
    state: &mut Test262AgentState,
    location: AtomicsWaitLocation,
    waiter: u64,
) -> Option<WaiterRegistration> {
    let queue = state.waiter_queues.get_mut(&location)?;
    let removed = queue
        .iter()
        .position(|registration| registration.id == waiter)
        .and_then(|position| queue.remove(position));
    if queue.is_empty() {
        state.waiter_queues.remove(&location);
    }
    removed
}
