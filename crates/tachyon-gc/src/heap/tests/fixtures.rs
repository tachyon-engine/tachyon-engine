use super::*;

pub(super) struct OtherPayload;

#[derive(Debug, Eq, PartialEq)]
pub(super) struct LargePayload {
    pub(super) _bytes: [u8; 70_000],
}

pub(super) struct ChainNode {
    pub(super) next: Option<GcRef<ChainNode>>,
}

pub(super) struct LargeEdgeNode {
    pub(super) _bytes: [u8; 70_000],
    pub(super) next: Option<GcRef<ChainNode>>,
}

pub(super) struct Leaf;

pub(super) struct Fanout {
    pub(super) edges: [Option<GcRef<Leaf>>; 300],
}

pub(super) struct WeakHolder {
    pub(super) target: WeakGcRef<ChainNode>,
}

pub(super) struct EphemeronHolder {
    pub(super) entry: Ephemeron<ChainNode>,
}

pub(super) struct FinalizationHolder {
    pub(super) registration: FinalizationRegistration<ChainNode>,
}

pub(super) struct PinnedPayload;

pub(super) struct ExternalPayload {
    pub(super) backing: Box<[u8]>,
}

pub(super) struct LargeExternalPayload {
    pub(super) _inline: [u8; 70_000],
    pub(super) backing: Box<[u8]>,
}

pub(super) struct ReportedExternalBytes(pub(super) usize);

pub(super) struct DropNode {
    pub(super) next: Option<GcRef<DropNode>>,
    pub(super) drops: Arc<AtomicUsize>,
}

pub(super) struct DropLarge {
    pub(super) _bytes: [u8; 70_000],
    pub(super) drops: Arc<AtomicUsize>,
}

pub(super) struct StressRoots {
    pub(super) stable: Vec<Value>,
    pub(super) nodes: Vec<GcRef<ChainNode>>,
}

impl StressRoots {
    pub(super) fn new() -> Self {
        Self {
            stable: Vec::with_capacity(8),
            nodes: Vec::with_capacity(16),
        }
    }
}

impl Trace for StressRoots {
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.stable.trace(tracer);
        self.nodes.trace(tracer);
    }
}

pub(super) struct DeterministicRng(pub(super) u64);

impl DeterministicRng {
    #[inline]
    pub(super) fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

impl Trace for OtherPayload {
    fn trace(&mut self, _: &mut dyn Tracer) {}
}

impl Trace for LargePayload {
    fn trace(&mut self, _: &mut dyn Tracer) {}
}

impl Trace for ChainNode {
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.next.trace(tracer);
    }
}

impl Trace for LargeEdgeNode {
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.next.trace(tracer);
    }
}

impl Trace for Leaf {
    fn trace(&mut self, _: &mut dyn Tracer) {}
}

impl Trace for Fanout {
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.edges.trace(tracer);
    }
}

impl Trace for WeakHolder {
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.target.trace(tracer);
    }
}

impl Trace for EphemeronHolder {
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.entry.trace(tracer);
    }
}

impl Trace for FinalizationHolder {
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.registration.trace(tracer);
    }
}

impl Trace for PinnedPayload {
    fn trace(&mut self, _: &mut dyn Tracer) {}
}

impl Trace for ExternalPayload {
    fn trace(&mut self, _: &mut dyn Tracer) {}
}

impl GcExternalMemory for ExternalPayload {
    fn external_memory_bytes(&self) -> usize {
        self.backing.len()
    }
}

impl Trace for LargeExternalPayload {
    fn trace(&mut self, _: &mut dyn Tracer) {}
}

impl GcExternalMemory for LargeExternalPayload {
    fn external_memory_bytes(&self) -> usize {
        self.backing.len()
    }
}

impl Trace for ReportedExternalBytes {
    fn trace(&mut self, _: &mut dyn Tracer) {}
}

impl GcExternalMemory for ReportedExternalBytes {
    fn external_memory_bytes(&self) -> usize {
        self.0
    }
}

impl Trace for DropNode {
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.next.trace(tracer);
    }
}

impl Drop for DropNode {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::Relaxed);
    }
}

impl Trace for DropLarge {
    fn trace(&mut self, _: &mut dyn Tracer) {}
}

impl Drop for DropLarge {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::Relaxed);
    }
}
