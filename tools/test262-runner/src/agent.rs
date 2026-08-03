//! Runner-owned Test262 agent cluster, parking registry, and worker lifecycle.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{Arc, Condvar, Mutex, mpsc},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use tachyon_vm::{
    AgentBroadcast, AgentHostProvider, AtomicsWaitLocation, AtomicsWaitResult,
    AtomicsWaiterProvider, HostProviderError,
};

use crate::tachyon::run_agent_worker;

const MAX_AGENT_SLEEP: Duration = Duration::from_secs(60);
const MAX_AGENT_WORKERS: u64 = 64;
const MAX_AGENT_REPORTS: usize = 4_096;
const INITIAL_AGENT_WORKER_CAPACITY: usize = 8;
const INITIAL_AGENT_REPORT_CAPACITY: usize = 16;
const INITIAL_AGENT_LOCATION_CAPACITY: usize = 8;

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

struct Test262AgentState {
    waiter_queues: HashMap<AtomicsWaitLocation, VecDeque<u64>>,
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
    started: Instant,
}

impl Test262AgentCluster {
    fn new() -> Self {
        Self {
            state: Mutex::new(Test262AgentState::new()),
            changed: Condvar::new(),
            workers: Mutex::new(Vec::with_capacity(INITIAL_AGENT_WORKER_CAPACITY)),
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

    fn cancel(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.cancelled = true;
            state.waiter_queues.clear();
            state.broadcast = None;
            self.changed.notify_all();
        }
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
        if let Some(queue) = state.waiter_queues.get_mut(&location) {
            while notified < count && queue.pop_front().is_some() {
                notified += 1;
            }
            if queue.is_empty() {
                state.waiter_queues.remove(&location);
            }
        }
        drop(state);
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
        state.next_waiter = state.next_waiter.wrapping_add(1);
        state
            .waiter_queues
            .entry(location)
            .or_default()
            .push_back(waiter);
        self.wait_loop(state, location, waiter, timeout)
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
                remove_waiter(&mut state, location, waiter);
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
                remove_waiter(&mut state, location, waiter);
                return Ok(AtomicsWaitResult::TimedOut);
            };
            let (next, elapsed) = self
                .cluster
                .changed
                .wait_timeout(state, remaining)
                .map_err(provider_failure)?;
            state = next;
            if elapsed.timed_out() && waiter_is_registered(&state, location, waiter) {
                remove_waiter(&mut state, location, waiter);
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
        .is_some_and(|queue| queue.contains(&waiter))
}

/// Removes one timed-out waiter without disturbing FIFO order for remaining agents.
fn remove_waiter(state: &mut Test262AgentState, location: AtomicsWaitLocation, waiter: u64) {
    let Some(queue) = state.waiter_queues.get_mut(&location) else {
        return;
    };
    if let Some(position) = queue.iter().position(|candidate| *candidate == waiter) {
        queue.remove(position);
    }
    if queue.is_empty() {
        state.waiter_queues.remove(&location);
    }
}
