use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use tachyon_compiler::{CompileOptions, Compiler, MediaType, SourceId, SourceName, SourceText};

use super::{
    eval::{dynamic_function_callback, eval_script_callback},
    *,
};

const HOST_AGENT_SOURCE: &str = r#"
const source = "worker-source";
const sab = new SharedArrayBuffer(8);
$262.agent._start(source);
$262.agent._broadcast(sab, 7n);
const packet = $262.agent._receiveBroadcast();
$262.agent._report("ready");
$262.agent._sleep(5);
const now = $262.agent._monotonicNow();
$262.agent._leaving();
const hostReport = $262.agent._getReport();
source.length === 13 &&
  new Int32Array(packet.buffer).length === 2 &&
  packet.value === 7n &&
  now === 17 &&
  hostReport === "host-report";
"#;

#[derive(Default)]
struct RecordingState {
    starts: Vec<Box<[u16]>>,
    broadcast: Option<AgentBroadcast>,
    reports: Vec<Box<[u16]>>,
    sleeps: Vec<f64>,
    leaving: usize,
    incoming: VecDeque<Box<[u16]>>,
}

struct RecordingAgentHost {
    state: Arc<Mutex<RecordingState>>,
}

impl AgentHostProvider for RecordingAgentHost {
    fn start(&mut self, source: Box<[u16]>) -> Result<(), HostProviderError> {
        self.state.lock().unwrap().starts.push(source);
        Ok(())
    }

    fn broadcast(&mut self, message: AgentBroadcast) -> Result<(), HostProviderError> {
        self.state.lock().unwrap().broadcast = Some(message);
        Ok(())
    }

    fn receive_broadcast(&mut self) -> Result<AgentBroadcast, HostProviderError> {
        self.state
            .lock()
            .unwrap()
            .broadcast
            .clone()
            .ok_or(HostProviderError::Unavailable)
    }

    fn report(&mut self, message: Box<[u16]>) -> Result<(), HostProviderError> {
        self.state.lock().unwrap().reports.push(message);
        Ok(())
    }

    fn get_report(&mut self) -> Result<Option<Box<[u16]>>, HostProviderError> {
        Ok(self.state.lock().unwrap().incoming.pop_front())
    }

    fn sleep(&mut self, milliseconds: f64) -> Result<(), HostProviderError> {
        self.state.lock().unwrap().sleeps.push(milliseconds);
        Ok(())
    }

    fn monotonic_now(&mut self) -> Result<f64, HostProviderError> {
        Ok(17.0)
    }

    fn leaving(&mut self) -> Result<(), HostProviderError> {
        self.state.lock().unwrap().leaving += 1;
        Ok(())
    }
}

#[test]
fn host_agent_surface_supports_every_dispatch_batch() {
    assert_host_agent::<1>(false);
    assert_host_agent::<2>(false);
    assert_host_agent::<4>(false);
    assert_host_agent::<8>(false);
    assert_host_agent::<16>(false);
}

#[test]
fn host_agent_surface_survives_forced_major_collection() {
    assert_host_agent::<8>(true);
}

/// Executes every raw host-agent operation and verifies owned values cross the provider boundary.
fn assert_host_agent<const N: usize>(forced_major: bool) {
    let state = Arc::new(Mutex::new(RecordingState {
        incoming: VecDeque::from(["host-report".encode_utf16().collect()]),
        ..RecordingState::default()
    }));
    let mut isolate = Isolate::new_with_host_providers(
        IsolateConfig::new(
            AtomTableConfig::new(4_096, 4 * 1024 * 1024, AtomHashSeed::new(1, 2)),
            HeapLimit::new(64 * SPAN_SIZE_BYTES),
            StackLimits::new(128, 16_384),
            RealmLimits::new(64, 4_096),
        ),
        HostProviders::new().with_agent_host(RecordingAgentHost {
            state: Arc::clone(&state),
        }),
    )
    .expect("host-agent isolate initializes");
    isolate
        .install_realm_hooks(eval_script_callback, dynamic_function_callback)
        .expect("host-agent hooks install");
    if forced_major {
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
    }
    let module = Compiler
        .compile(
            SourceText::new(
                SourceId::new(9_400 + N as u32),
                SourceName::new("host-agent"),
                MediaType::JavaScript,
                Arc::from(HOST_AGENT_SOURCE),
            ),
            CompileOptions::default(),
        )
        .expect("host-agent source compiles");
    let outcome = isolate
        .execute_with_batch::<N>(
            &module,
            ExecutionBudget {
                fuel: 65_536,
                quantum: 65_536,
            },
        )
        .expect("host-agent source executes");
    assert!(
        matches!(
            outcome,
            RunOutcome::Completed(value) if value.as_immediate() == Some(Immediate::True)
        ),
        "dispatch batch {N}, forced_major={forced_major} returned {outcome:?}"
    );
    let state = state.lock().unwrap();
    assert_eq!(String::from_utf16_lossy(&state.starts[0]), "worker-source");
    assert_eq!(String::from_utf16_lossy(&state.reports[0]), "ready");
    assert_eq!(state.sleeps, [5.0]);
    assert_eq!(state.leaving, 1);
    assert!(state.broadcast.is_some());
}
