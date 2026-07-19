//! Interpreter dispatch-loop tuning knobs.

/// Number of opcodes fetched by one outer interpreter-loop iteration.
///
/// Unit: opcodes; valid range: one of 1, 2, 4, 8, or 16 and non-zero. The verified kernel retains
/// local PC/register cursors across these opcodes and publishes PC once at a batch or slow-exit
/// boundary. Bounded execution still consumes exact fuel and quantum per opcode. The initial value
/// favors throughput without selecting the largest tested monomorphization. Correctness is paired
/// across all five candidates using arithmetic, branches, calls, throws, properties, logical
/// expressions, and construction. Formal per-architecture tuning must compare throughput, text
/// size, I-cache behavior, and cold start on the fixed corpus.
pub(crate) const DEFAULT_DISPATCH_BATCH: usize = 8;

const _: () = assert!(DEFAULT_DISPATCH_BATCH > 0);
const _: () = assert!(matches!(DEFAULT_DISPATCH_BATCH, 1 | 2 | 4 | 8 | 16));
