#![deny(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    clippy::print_stderr,
    clippy::print_stdout,
    unsafe_op_in_unsafe_fn
)]
//! Isolate, fiber, interpreter, and ECMAScript builtin execution machinery.
//!
//! This crate intentionally has no host I/O surface.

mod atom;
mod finalization;
mod object;
mod string;
mod tuning;

pub use atom::{AtomHashSeed, AtomId, AtomTable, AtomTableConfig, AtomTableError, AtomTableStats};

pub use finalization::{
    FinalizationCleanupJob, FinalizationJobQueueStats, FinalizationSafepointError,
    FinalizationSafepointStats,
};
pub use object::ShapeError;
pub use string::{JsString, JsStringView, StringAllocationError, StringRepresentationTag};

use core::{cell::Cell, num::NonZeroU32};

use tachyon_bytecode::{
    BytecodeConstant, CompiledModule, FunctionId, FunctionKind, FunctionLayout, HandlerEntry,
    HandlerKind, Opcode, RegisterId, WordOffset, decode_instruction,
};
use tachyon_gc::{
    AllocationSpace, GcRef, GcType, Heap, HeapAllocationError, HeapLimit, HeapReferenceError,
    ManagedAllocationError, NoGcBorrowError, RootError, Trace, Tracer, TypeRegistrationError,
    TypeRegistry,
};
use tachyon_value::{Immediate, Value};

use object::{OrdinaryObject, PropertyAttributes, PropertyStorage, ShapeId, ShapeTable};

/// Shareable immutable engine configuration. Host services deliberately do not live here.
#[derive(Clone, Copy, Debug, Default)]
pub struct Engine;

/// Mandatory isolate resource and entropy configuration; production has no fixed hash seed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IsolateConfig {
    atom_table: AtomTableConfig,
    heap_limit: HeapLimit,
    stack_limits: StackLimits,
    realm_limits: RealmLimits,
}

impl IsolateConfig {
    #[must_use]
    pub const fn new(
        atom_table: AtomTableConfig,
        heap_limit: HeapLimit,
        stack_limits: StackLimits,
        realm_limits: RealmLimits,
    ) -> Self {
        Self {
            atom_table,
            heap_limit,
            stack_limits,
            realm_limits,
        }
    }
}

/// Host hard limits for isolate-retained code and global object bindings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RealmLimits {
    max_loaded_modules: u32,
    max_global_bindings: u32,
    max_shapes: u32,
}

impl RealmLimits {
    #[must_use]
    pub const fn new(max_loaded_modules: u32, max_global_bindings: u32) -> Self {
        Self {
            max_loaded_modules,
            max_global_bindings,
            max_shapes: max_global_bindings,
        }
    }

    /// Overrides the hidden-class hard limit when object churn differs from global binding count.
    #[must_use]
    pub const fn with_max_shapes(mut self, max_shapes: u32) -> Self {
        self.max_shapes = max_shapes;
        self
    }
}

/// Host-provided hard bounds for explicit JavaScript frames and their register windows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StackLimits {
    max_frames: u32,
    max_registers: u32,
}

impl StackLimits {
    #[must_use]
    pub const fn new(max_frames: u32, max_registers: u32) -> Self {
        Self {
            max_frames,
            max_registers,
        }
    }
}

/// A per-execution bound; fuel is a hard cap while quantum bounds one synchronous interpreter turn.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExecutionBudget {
    pub fuel: u64,
    pub quantum: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunOutcome {
    Completed(Value),
    Thrown(Value),
    BudgetExhausted,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExecutionError {
    InvalidDispatchBatch { batch: usize },
    MissingEntryFunction(FunctionId),
    RegisterWindowTooLarge(u32),
    HandlerStackTooLarge(u32),
    CompletionStackTooLarge(u32),
    FrameAllocationFailed,
    RegisterAllocationFailed,
    HandlerAllocationFailed,
    CompletionAllocationFailed,
    DecodeInvariant(WordOffset),
    UnsupportedOpcode(Opcode),
    UnsupportedConstant(u32),
    InvalidRegister(RegisterId),
    NonCallable(Value),
    NonConstructor(Value),
    InvalidInstanceofPrototype(Value),
    HeapAllocation(ManagedAllocationError),
    HeapReference(HeapReferenceError),
    Root(RootError),
    NoGcBorrow(NoGcBorrowError),
    MissingPendingException,
    UnsupportedExceptionHandler(HandlerKind),
    CallStackLimit { limit: u32 },
    RegisterStackLimit { limit: u32, requested: u32 },
    LoadedModuleLimit { limit: u32 },
    LoadedCodeAllocationFailed,
    ScopeNameAllocationFailed,
    ScopeNameAtom(AtomTableError),
    ScopeNameString(StringAllocationError),
    ConstantValueAllocationFailed,
    ConstantString(StringAllocationError),
    PropertyKeyAtom(AtomTableError),
    PropertyKeyString(StringAllocationError),
    UnsupportedPropertyKey(Value),
    UnsupportedTypeof(Value),
    InvalidCode(CodeId),
    InvalidScopeName { code: CodeId, scope_name: u32 },
    UnresolvedBinding(AtomId),
    ReadOnlyBinding(AtomId),
    GlobalBindingLimit { limit: u32 },
    GlobalBindingAllocationFailed,
    GlobalBindingIndexAllocationFailed,
    Shape(ShapeError),
    PropertyStorageAllocationFailed,
    NotObject(Value),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IsolateCreationError {
    TypeRegistration(TypeRegistrationError),
    Shape(ShapeError),
    String(StringAllocationError),
    HeapAllocation(HeapAllocationError),
}

/// A future GC-managed lexical environment. Its concrete payload arrives with M5.
#[derive(Debug)]
struct Environment;

impl Trace for Environment {
    fn trace(&mut self, _: &mut dyn Tracer) {}
}

/// Minimal callable payload; properties and captured environments extend this object in M5.
#[derive(Clone, Copy, Debug)]
struct FunctionObject {
    code: CodeId,
    function: FunctionId,
    environment: Option<GcRef<Environment>>,
    function_prototype: Option<Value>,
    ordinary: OrdinaryObject,
}

#[derive(Clone, Copy)]
enum ObjectReceiver {
    Ordinary(GcRef<OrdinaryObject>),
    Function(GcRef<FunctionObject>),
}

impl ObjectReceiver {
    #[inline(always)]
    fn value(self) -> Value {
        match self {
            Self::Ordinary(object) => Value::from_heap_ref(object.raw()),
            Self::Function(function) => Value::from_heap_ref(function.raw()),
        }
    }
}

#[derive(Clone, Copy)]
struct ResolvedCallTarget {
    object: FunctionObject,
    layout: FunctionLayout,
    kind: FunctionKind,
}

#[derive(Clone, Copy)]
struct CallSite {
    caller_base: u32,
    destination: u32,
    callee_register: u32,
    argument_count: u32,
    this_value: Value,
    new_target: Value,
    construct_receiver: Option<Value>,
    call_site: WordOffset,
}

impl Trace for FunctionObject {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.environment.trace(tracer);
        self.function_prototype.trace(tracer);
        self.ordinary.trace(tracer);
    }
}

#[derive(Clone, Copy)]
struct VmTypes {
    function: GcType<FunctionObject>,
    ordinary_object: GcType<OrdinaryObject>,
    property_storage: GcType<PropertyStorage>,
    string: GcType<JsString>,
}

#[derive(Clone, Copy, Debug, Default)]
struct IntrinsicPropertyAtoms {
    prototype: Option<AtomId>,
    constructor: Option<AtomId>,
}

/// An isolate-local immutable-code index; zero stays reserved for niche optimization and validation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct CodeId(NonZeroU32);

impl CodeId {
    fn from_index(index: usize) -> Option<Self> {
        u32::try_from(index)
            .ok()
            .and_then(|index| index.checked_add(1))
            .and_then(NonZeroU32::new)
            .map(Self)
    }

    const fn index(self) -> usize {
        (self.0.get() - 1) as usize
    }
}

const _: [(); 4] = [(); core::mem::size_of::<CodeId>()];
const _: [(); 4] = [(); core::mem::size_of::<Option<CodeId>>()];

#[derive(Debug)]
struct LoadedCode {
    module: CompiledModule,
    scope_resolutions: Box<[ScopeResolution]>,
    constant_values: Box<[Option<Value>]>,
}

impl Trace for LoadedCode {
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        for value in self.constant_values.iter_mut().flatten() {
            value.trace(tracer);
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct ScopeResolution {
    atom: AtomId,
    global_slot: Option<GlobalSlotId>,
}

#[derive(Clone, Copy, Debug)]
struct GlobalBinding {
    name: AtomId,
    value: Value,
}

/// A stable isolate-local index into one realm's global binding storage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
struct GlobalSlotId(NonZeroU32);

impl GlobalSlotId {
    fn from_index(index: usize) -> Option<Self> {
        u32::try_from(index)
            .ok()
            .and_then(|index| index.checked_add(1))
            .and_then(NonZeroU32::new)
            .map(Self)
    }

    const fn index(self) -> usize {
        (self.0.get() - 1) as usize
    }
}

const _: [(); 4] = [(); core::mem::size_of::<GlobalSlotId>()];
const _: [(); 4] = [(); core::mem::size_of::<Option<GlobalSlotId>>()];

#[derive(Debug)]
struct Realm {
    global_bindings: Vec<GlobalBinding>,
    global_slots_by_atom: Vec<Option<GlobalSlotId>>,
    typeof_strings: TypeofStrings,
    limits: RealmLimits,
}

impl Realm {
    fn new(limits: RealmLimits, typeof_strings: TypeofStrings) -> Self {
        Self {
            global_bindings: Vec::new(),
            global_slots_by_atom: Vec::new(),
            typeof_strings,
            limits,
        }
    }

    #[inline(always)]
    fn get_slot(&self, slot: GlobalSlotId) -> Option<Value> {
        self.global_bindings
            .get(slot.index())
            .map(|binding| binding.value)
    }

    #[inline(always)]
    fn set_slot(&mut self, slot: GlobalSlotId, value: Value) {
        self.global_bindings[slot.index()].value = value;
    }

    /// Updates an existing slot or atomically publishes one after both backing reserves succeed.
    fn set(&mut self, name: AtomId, value: Value) -> Result<(), ExecutionError> {
        if let Some(slot) = self.resolve(name) {
            self.set_slot(slot, value);
            return Ok(());
        }
        if self.global_bindings.len() >= self.limits.max_global_bindings as usize {
            return Err(ExecutionError::GlobalBindingLimit {
                limit: self.limits.max_global_bindings,
            });
        }
        let required_slots = (name.index() as usize)
            .checked_add(1)
            .ok_or(ExecutionError::GlobalBindingIndexAllocationFailed)?;
        let additional_slots = required_slots.saturating_sub(self.global_slots_by_atom.len());
        self.global_slots_by_atom
            .try_reserve_exact(additional_slots)
            .map_err(|_| ExecutionError::GlobalBindingIndexAllocationFailed)?;
        self.global_bindings
            .try_reserve_exact(1)
            .map_err(|_| ExecutionError::GlobalBindingAllocationFailed)?;
        self.global_slots_by_atom.resize(required_slots, None);
        let slot = GlobalSlotId::from_index(self.global_bindings.len())
            .ok_or(ExecutionError::GlobalBindingLimit { limit: u32::MAX })?;
        self.global_bindings.push(GlobalBinding { name, value });
        self.global_slots_by_atom[name.index() as usize] = Some(slot);
        Ok(())
    }

    #[inline(always)]
    fn resolve(&self, name: AtomId) -> Option<GlobalSlotId> {
        let slot = self
            .global_slots_by_atom
            .get(name.index() as usize)
            .copied()
            .flatten()?;
        debug_assert_eq!(self.global_bindings[slot.index()].name, name);
        Some(slot)
    }
}

impl Trace for Realm {
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        for binding in &mut self.global_bindings {
            binding.value.trace(tracer);
        }
        self.typeof_strings.trace(tracer);
    }
}

#[derive(Clone, Copy, Debug)]
struct TypeofStrings {
    undefined: Value,
    object: Value,
    boolean: Value,
    number: Value,
    string: Value,
    function: Value,
    symbol: Value,
    bigint: Value,
}

impl Trace for TypeofStrings {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.undefined.trace(tracer);
        self.object.trace(tracer);
        self.boolean.trace(tracer);
        self.number.trace(tracer);
        self.string.trace(tracer);
        self.function.trace(tracer);
        self.symbol.trace(tracer);
        self.bigint.trace(tracer);
    }
}

impl TypeofStrings {
    /// Allocates the complete spec-fixed typeof vocabulary once before the isolate becomes visible.
    fn allocate(
        heap: &mut Heap,
        string_type: GcType<JsString>,
    ) -> Result<Self, IsolateCreationError> {
        Ok(Self {
            undefined: allocate_initial_string(heap, string_type, b"undefined")?,
            object: allocate_initial_string(heap, string_type, b"object")?,
            boolean: allocate_initial_string(heap, string_type, b"boolean")?,
            number: allocate_initial_string(heap, string_type, b"number")?,
            string: allocate_initial_string(heap, string_type, b"string")?,
            function: allocate_initial_string(heap, string_type, b"function")?,
            symbol: allocate_initial_string(heap, string_type, b"symbol")?,
            bigint: allocate_initial_string(heap, string_type, b"bigint")?,
        })
    }
}

fn allocate_initial_string(
    heap: &mut Heap,
    string_type: GcType<JsString>,
    bytes: &[u8],
) -> Result<Value, IsolateCreationError> {
    let string = JsString::try_from_latin1(bytes).map_err(IsolateCreationError::String)?;
    let reference = heap
        .try_allocate_external(string_type, 0, string, AllocationSpace::Old)
        .map_err(IsolateCreationError::HeapAllocation)?;
    Ok(Value::from_heap_ref(reference.raw()))
}

struct VmRoots<'a> {
    fiber: &'a mut Fiber,
    finalization_jobs: &'a mut finalization::FinalizationJobs,
    realm: &'a mut Realm,
    loaded_code: &'a mut Vec<LoadedCode>,
}

struct PropertyMutationRoots<'a> {
    vm: VmRoots<'a>,
    receiver: Value,
}

struct PrototypeInitializationRoots<'a> {
    vm: VmRoots<'a>,
    function: Value,
}

impl Trace for PropertyMutationRoots<'_> {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.receiver.trace(tracer);
    }
}

impl Trace for PrototypeInitializationRoots<'_> {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.function.trace(tracer);
    }
}

impl Trace for VmRoots<'_> {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.fiber.trace_roots(tracer);
        self.finalization_jobs.trace(tracer);
        self.realm.trace(tracer);
        for code in self.loaded_code.iter_mut() {
            code.trace(tracer);
        }
    }
}

struct CodeLoadRoots<'a> {
    vm: VmRoots<'a>,
    constant_values: &'a mut Vec<Option<Value>>,
}

impl Trace for CodeLoadRoots<'_> {
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        for value in self.constant_values.iter_mut().flatten() {
            value.trace(tracer);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Strictness {
    Sloppy,
    Strict,
}

/// One explicit JavaScript activation. Rust stack frames never represent JavaScript calls.
#[derive(Clone, Copy, Debug)]
struct Frame {
    code: CodeId,
    function: FunctionId,
    pc: WordOffset,
    base: u32,
    environment: Option<GcRef<Environment>>,
    return_register: Option<RegisterId>,
    this_value: Value,
    new_target: Value,
    construct_receiver: Option<Value>,
    strictness: Strictness,
    argument_base: u32,
    argument_count: u32,
    handler_base: u32,
    completion_base: u32,
    call_site: Option<WordOffset>,
}

/// The dynamic handler state selected from immutable bytecode handler metadata.
#[derive(Clone, Copy, Debug)]
struct ActiveHandler {
    handler_index: u32,
    frame_depth: u32,
    environment_depth: u32,
}

/// Abrupt completions are data, so throw/finally never need Rust stack unwinding.
#[derive(Clone, Copy, Debug)]
#[allow(dead_code)] // Populated by Throw/finally lowering after handler dispatch is implemented.
enum Completion {
    Return(Value),
    Throw(Value),
}

#[derive(Debug, Default)]
struct Fiber {
    frames: Vec<Frame>,
    registers: Vec<Value>,
    handlers: Vec<ActiveHandler>,
    completions: Vec<Completion>,
    pending_exception: Option<Value>,
}

impl Fiber {
    /// Traces every mutable reference reachable from an active, yielded, or suspended fiber.
    ///
    /// Frame control indices are validated when handlers are installed. They do not themselves
    /// own heap references, while registers, frame context, and abrupt completion payloads do.
    fn trace_roots(&mut self, tracer: &mut dyn Tracer) {
        self.registers.trace(tracer);
        for frame in &mut self.frames {
            frame.environment.trace(tracer);
            frame.this_value.trace(tracer);
            frame.new_target.trace(tracer);
            frame.construct_receiver.trace(tracer);
            if let Some(return_register) = frame.return_register {
                debug_assert!((return_register.index() as usize) < self.registers.len());
            }
            debug_assert!(
                frame
                    .argument_base
                    .checked_add(frame.argument_count)
                    .is_some_and(|end| end as usize <= self.registers.len())
            );
            let _is_strict = matches!(frame.strictness, Strictness::Strict);
        }
        for handler in &self.handlers {
            debug_assert!(
                usize::try_from(handler.frame_depth).is_ok_and(|depth| depth <= self.frames.len())
            );
            debug_assert!(
                usize::try_from(handler.environment_depth)
                    .is_ok_and(|depth| depth <= self.frames.len())
            );
            let _ = handler.handler_index;
        }
        for completion in &mut self.completions {
            match completion {
                Completion::Return(value) | Completion::Throw(value) => value.trace(tracer),
            }
        }
        self.pending_exception.trace(tracer);
    }
}

/// A single-thread-owned ECMAScript execution state; `Cell` intentionally makes it `!Sync`.
pub struct Isolate {
    fiber: Fiber,
    finalization_jobs: finalization::FinalizationJobs,
    atoms: AtomTable,
    shapes: ShapeTable,
    realm: Realm,
    loaded_code: Vec<LoadedCode>,
    heap: Heap,
    types: VmTypes,
    intrinsic_property_atoms: IntrinsicPropertyAtoms,
    stack_limits: StackLimits,
    _not_sync: Cell<()>,
}

impl Isolate {
    /// Registers VM payload descriptors before constructing an otherwise empty isolate heap.
    pub fn new(config: IsolateConfig) -> Result<Self, IsolateCreationError> {
        let mut registry = TypeRegistry::new();
        let types = VmTypes {
            function: registry
                .try_register("FunctionObject")
                .map_err(IsolateCreationError::TypeRegistration)?,
            ordinary_object: registry
                .try_register("OrdinaryObject")
                .map_err(IsolateCreationError::TypeRegistration)?,
            property_storage: registry
                .try_register("PropertyStorage")
                .map_err(IsolateCreationError::TypeRegistration)?,
            string: registry
                .try_register("JsString")
                .map_err(IsolateCreationError::TypeRegistration)?,
        };
        let shapes =
            ShapeTable::new(config.realm_limits.max_shapes).map_err(IsolateCreationError::Shape)?;
        let mut heap = Heap::new(config.heap_limit, registry);
        let typeof_strings = TypeofStrings::allocate(&mut heap, types.string)?;
        Ok(Self {
            fiber: Fiber::default(),
            finalization_jobs: finalization::FinalizationJobs::new(),
            atoms: AtomTable::new(config.atom_table),
            shapes,
            realm: Realm::new(config.realm_limits, typeof_strings),
            loaded_code: Vec::new(),
            heap,
            types,
            intrinsic_property_atoms: IntrinsicPropertyAtoms::default(),
            stack_limits: config.stack_limits,
            _not_sync: Cell::new(()),
        })
    }

    #[must_use]
    pub const fn atoms(&self) -> &AtomTable {
        &self.atoms
    }

    pub const fn atoms_mut(&mut self) -> &mut AtomTable {
        &mut self.atoms
    }

    /// Allocates an empty ordinary object with a caller-selected prototype through managed GC.
    fn create_ordinary_object(&mut self) -> Result<Value, ExecutionError> {
        self.create_ordinary_object_with_prototype(Value::from_immediate(Immediate::Null))
    }

    /// Keeps a prototype edge in the pending payload so pre-allocation collection can rewrite it.
    fn create_ordinary_object_with_prototype(
        &mut self,
        prototype: Value,
    ) -> Result<Value, ExecutionError> {
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
        };
        let object = self
            .heap
            .try_allocate_with_gc(
                self.types.ordinary_object,
                0,
                0,
                OrdinaryObject {
                    shape: ShapeId::EMPTY,
                    storage: None,
                    prototype,
                },
                AllocationSpace::Young,
                roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        Ok(Value::from_heap_ref(object.raw()))
    }

    /// Walks ordinary prototype links without allocating or invoking accessor/exotic behavior.
    fn get_data_property(
        &mut self,
        receiver: Value,
        key: AtomId,
    ) -> Result<Option<Value>, ExecutionError> {
        let mut current = receiver;
        loop {
            if self.is_function_prototype_property(current, key) {
                self.intrinsic_property_atoms.prototype = Some(key);
                return self.ensure_function_prototype(current).map(Some);
            }
            let (_, snapshot) = self.object_snapshot(current)?;
            if let Some(value) = self.data_property_from_snapshot(snapshot, key)? {
                return Ok(Some(value));
            }
            if snapshot.prototype.as_immediate() == Some(Immediate::Null) {
                return Ok(None);
            }
            if !self.is_object_value(snapshot.prototype) {
                return Err(ExecutionError::NotObject(snapshot.prototype));
            }
            current = snapshot.prototype;
        }
    }

    /// Reads a known ordinary snapshot's fixed slot without repeating receiver classification.
    fn data_property_from_snapshot(
        &mut self,
        snapshot: OrdinaryObject,
        key: AtomId,
    ) -> Result<Option<Value>, ExecutionError> {
        let Some(property) = self.shapes.lookup(snapshot.shape, key) else {
            return Ok(None);
        };
        debug_assert_eq!(property.attributes, PropertyAttributes::DEFAULT_DATA);
        let storage = snapshot
            .storage
            .expect("a non-empty shape always owns property storage");
        self.heap.with_running_scope(|scope| {
            let local = scope.root(storage).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(local, self.types.property_storage)
                    .map_err(ExecutionError::NoGcBorrow)
                    .map(|storage| storage.slots.get(property.slot as usize).copied())
            })
        })
    }

    fn prototype_atom(&mut self) -> Result<AtomId, ExecutionError> {
        if let Some(atom) = self.intrinsic_property_atoms.prototype {
            return Ok(atom);
        }
        let string =
            JsString::try_from_latin1(b"prototype").map_err(ExecutionError::PropertyKeyString)?;
        let atom = self
            .atoms
            .try_intern(string)
            .map_err(ExecutionError::PropertyKeyAtom)?;
        self.intrinsic_property_atoms.prototype = Some(atom);
        Ok(atom)
    }

    fn constructor_atom(&mut self) -> Result<AtomId, ExecutionError> {
        if let Some(atom) = self.intrinsic_property_atoms.constructor {
            return Ok(atom);
        }
        let string =
            JsString::try_from_latin1(b"constructor").map_err(ExecutionError::PropertyKeyString)?;
        let atom = self
            .atoms
            .try_intern(string)
            .map_err(ExecutionError::PropertyKeyAtom)?;
        self.intrinsic_property_atoms.constructor = Some(atom);
        Ok(atom)
    }

    #[inline(always)]
    fn is_function_prototype_property(&self, receiver: Value, key: AtomId) -> bool {
        let Some(raw) = receiver.as_heap_ref() else {
            return false;
        };
        if self
            .heap
            .checked_reference(raw, self.types.function)
            .is_err()
        {
            return false;
        }
        self.intrinsic_property_atoms.prototype == Some(key)
            || self
                .atoms
                .get(key)
                .is_some_and(|name| name.equals_latin1(b"prototype"))
    }

    /// Materializes the spec-visible function prototype only on first observation or construction.
    fn ensure_function_prototype(&mut self, function: Value) -> Result<Value, ExecutionError> {
        let raw = function
            .as_heap_ref()
            .ok_or(ExecutionError::NonCallable(function))?;
        let reference = self
            .heap
            .checked_reference(raw, self.types.function)
            .map_err(|_| ExecutionError::NonCallable(function))?;
        let existing = self.heap.with_running_scope(|scope| {
            let function = scope.root(reference).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(function, self.types.function)
                    .map(|function| function.function_prototype)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })?;
        if let Some(prototype) = existing {
            return Ok(prototype);
        }
        self.materialize_function_prototype(function)
    }

    /// Allocates a one-slot constructor object, then publishes the lazy function edge with a barrier.
    fn materialize_function_prototype(&mut self, function: Value) -> Result<Value, ExecutionError> {
        let constructor_atom = self.constructor_atom()?;
        let shape = self
            .shapes
            .transition_add(
                ShapeId::EMPTY,
                constructor_atom,
                PropertyAttributes::DEFAULT_DATA,
            )
            .map_err(ExecutionError::Shape)?;
        let mut roots = PrototypeInitializationRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                finalization_jobs: &mut self.finalization_jobs,
                realm: &mut self.realm,
                loaded_code: &mut self.loaded_code,
            },
            function,
        };
        let storage = self
            .heap
            .try_allocate_external_with_gc(
                self.types.property_storage,
                0,
                PropertyStorage {
                    slots: Box::new([roots.function]),
                },
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        let prototype = self
            .heap
            .try_allocate_with_gc(
                self.types.ordinary_object,
                0,
                0,
                OrdinaryObject {
                    shape,
                    storage: Some(storage),
                    prototype: Value::from_immediate(Immediate::Null),
                },
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        let function = roots.function;
        self.set_function_prototype(function, Value::from_heap_ref(prototype.raw()))?;
        Ok(Value::from_heap_ref(prototype.raw()))
    }

    /// Replaces the inline function prototype slot and records its possible young edge.
    fn set_function_prototype(
        &mut self,
        function: Value,
        prototype: Value,
    ) -> Result<(), ExecutionError> {
        let raw = function
            .as_heap_ref()
            .ok_or(ExecutionError::NonCallable(function))?;
        let reference = self
            .heap
            .checked_reference(raw, self.types.function)
            .map_err(|_| ExecutionError::NonCallable(function))?;
        self.heap.with_running_scope(|scope| {
            let function = scope.root(reference).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let object = no_gc
                    .borrow_mut(function, self.types.function)
                    .map_err(ExecutionError::NoGcBorrow)?;
                object.function_prototype = Some(prototype);
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_value_barrier(function, prototype)
                .map_err(ExecutionError::HeapReference)?;
            Ok(())
        })
    }

    /// Implements ordinary HasInstance over the current constructor prototype and object chain.
    fn ordinary_instance_of(
        &mut self,
        value: Value,
        constructor: Value,
    ) -> Result<bool, ExecutionError> {
        let raw = constructor
            .as_heap_ref()
            .ok_or(ExecutionError::NonCallable(constructor))?;
        self.heap
            .checked_reference(raw, self.types.function)
            .map_err(|_| ExecutionError::NonCallable(constructor))?;
        let prototype_atom = self.prototype_atom()?;
        let prototype = self.get_data_property(constructor, prototype_atom)?.ok_or(
            ExecutionError::InvalidInstanceofPrototype(Value::from_immediate(Immediate::Undefined)),
        )?;
        if !self.is_object_value(prototype) {
            return Err(ExecutionError::InvalidInstanceofPrototype(prototype));
        }
        if !self.is_object_value(value) {
            return Ok(false);
        }
        let (_, mut snapshot) = self.object_snapshot(value)?;
        loop {
            let candidate = snapshot.prototype;
            if candidate.as_immediate() == Some(Immediate::Null) {
                return Ok(false);
            }
            if candidate == prototype {
                return Ok(true);
            }
            let (_, next) = self.object_snapshot(candidate)?;
            snapshot = next;
        }
    }

    /// Updates an existing slot in place or publishes an exactly sized replacement backing.
    fn set_own_data_property(
        &mut self,
        receiver: Value,
        key: AtomId,
        value: Value,
    ) -> Result<(), ExecutionError> {
        if self.is_function_prototype_property(receiver, key) {
            self.intrinsic_property_atoms.prototype = Some(key);
            return self.set_function_prototype(receiver, value);
        }
        let (object, snapshot) = self.object_snapshot(receiver)?;
        if let Some(property) = self.shapes.lookup(snapshot.shape, key) {
            debug_assert_eq!(property.attributes, PropertyAttributes::DEFAULT_DATA);
            return self.update_property_slot(snapshot, property.slot, value);
        }
        self.add_property_slot(object, snapshot, key, value)
    }

    /// Mutates a fixed existing slot and publishes its potential young edge to the barrier.
    fn update_property_slot(
        &mut self,
        snapshot: OrdinaryObject,
        slot: u32,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let storage = snapshot
            .storage
            .expect("an existing property slot always has storage");
        self.heap.with_running_scope(|scope| {
            let storage_local = scope.root(storage).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let storage = no_gc
                    .borrow_mut(storage_local, self.types.property_storage)
                    .map_err(ExecutionError::NoGcBorrow)?;
                storage.slots[slot as usize] = value;
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_value_barrier(storage_local, value)
                .map_err(ExecutionError::HeapReference)?;
            Ok(())
        })
    }

    /// Copies old slots into a traced pending backing, allocates it, then switches the object edge.
    fn add_property_slot(
        &mut self,
        object: ObjectReceiver,
        snapshot: OrdinaryObject,
        key: AtomId,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let new_shape = self
            .shapes
            .transition_add(snapshot.shape, key, PropertyAttributes::DEFAULT_DATA)
            .map_err(ExecutionError::Shape)?;
        let new_length = self.shapes.property_count(new_shape) as usize;
        let mut slots = Vec::new();
        slots
            .try_reserve_exact(new_length)
            .map_err(|_| ExecutionError::PropertyStorageAllocationFailed)?;
        if let Some(storage) = snapshot.storage {
            self.heap.with_running_scope(|scope| {
                let local = scope.root(storage).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    let old = no_gc
                        .borrow(local, self.types.property_storage)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    slots.extend_from_slice(&old.slots);
                    Ok::<(), ExecutionError>(())
                })
            })?;
        }
        slots.push(value);
        debug_assert_eq!(slots.len(), new_length);
        let (storage, receiver) = {
            let mut roots = PropertyMutationRoots {
                vm: VmRoots {
                    fiber: &mut self.fiber,
                    finalization_jobs: &mut self.finalization_jobs,
                    realm: &mut self.realm,
                    loaded_code: &mut self.loaded_code,
                },
                receiver: object.value(),
            };
            let storage = self
                .heap
                .try_allocate_external_with_gc(
                    self.types.property_storage,
                    0,
                    PropertyStorage {
                        slots: slots.into_boxed_slice(),
                    },
                    AllocationSpace::Young,
                    &mut roots,
                )
                .map_err(ExecutionError::HeapAllocation)?;
            (storage, roots.receiver)
        };
        let (object, _) = self.object_snapshot(receiver)?;
        self.attach_property_storage(object, new_shape, storage)
    }

    /// Resolves either ordinary or callable payloads to their shared ordinary-property snapshot.
    #[inline(always)]
    fn object_snapshot(
        &mut self,
        value: Value,
    ) -> Result<(ObjectReceiver, OrdinaryObject), ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::NotObject(value))?;
        if let Ok(object) = self.heap.checked_reference(raw, self.types.ordinary_object) {
            let snapshot = self.heap.with_running_scope(|scope| {
                let local = scope.root(object).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(local, self.types.ordinary_object)
                        .copied()
                        .map_err(ExecutionError::NoGcBorrow)
                })
            })?;
            return Ok((ObjectReceiver::Ordinary(object), snapshot));
        }
        let function = self
            .heap
            .checked_reference(raw, self.types.function)
            .map_err(|_| ExecutionError::NotObject(value))?;
        let ordinary = self.heap.with_running_scope(|scope| {
            let local = scope.root(function).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(local, self.types.function)
                    .map(|function| function.ordinary)
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })?;
        Ok((ObjectReceiver::Function(function), ordinary))
    }

    /// Publishes a replacement storage edge through the receiver's concrete typed payload.
    fn attach_property_storage(
        &mut self,
        receiver: ObjectReceiver,
        shape: ShapeId,
        storage: GcRef<PropertyStorage>,
    ) -> Result<(), ExecutionError> {
        match receiver {
            ObjectReceiver::Ordinary(object) => self.heap.with_running_scope(|scope| {
                let object = scope.root(object).map_err(ExecutionError::Root)?;
                let storage_local = scope.root(storage).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    let object = no_gc
                        .borrow_mut(object, self.types.ordinary_object)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    object.shape = shape;
                    object.storage = Some(storage);
                    Ok::<(), ExecutionError>(())
                })?;
                scope
                    .write_barrier(object, storage_local)
                    .map_err(ExecutionError::HeapReference)?;
                Ok(())
            }),
            ObjectReceiver::Function(function) => self.heap.with_running_scope(|scope| {
                let function = scope.root(function).map_err(ExecutionError::Root)?;
                let storage_local = scope.root(storage).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    let function = no_gc
                        .borrow_mut(function, self.types.function)
                        .map_err(ExecutionError::NoGcBorrow)?;
                    function.ordinary.shape = shape;
                    function.ordinary.storage = Some(storage);
                    Ok::<(), ExecutionError>(())
                })?;
                scope
                    .write_barrier(function, storage_local)
                    .map_err(ExecutionError::HeapReference)?;
                Ok(())
            }),
        }
    }

    #[inline(always)]
    fn is_object_value(&self, value: Value) -> bool {
        let Some(raw) = value.as_heap_ref() else {
            return false;
        };
        self.heap
            .checked_reference(raw, self.types.ordinary_object)
            .is_ok()
            || self
                .heap
                .checked_reference(raw, self.types.function)
                .is_ok()
    }

    /// Enumerates this isolate's fiber roots for a stop-the-world collection safepoint.
    ///
    /// The collector supplies a rewrite-capable tracer. This API does not resolve logical addresses
    /// or borrow heap objects, so it remains valid across non-moving collection phases.
    pub fn trace_roots(&mut self, tracer: &mut dyn Tracer) {
        self.fiber.trace_roots(tracer);
        self.finalization_jobs.trace(tracer);
        self.realm.trace(tracer);
        for code in &mut self.loaded_code {
            code.trace(tracer);
        }
    }

    /// Starts the module entry function with one checked register reservation before opcode dispatch.
    pub fn execute(
        &mut self,
        module: &CompiledModule,
        budget: ExecutionBudget,
    ) -> Result<RunOutcome, ExecutionError> {
        let code = self.load_module(module)?;
        self.execute_loaded(code, budget)
    }

    /// Resolves immutable scope names once and publishes one bounded isolate-local code entry.
    pub fn load_module(&mut self, module: &CompiledModule) -> Result<CodeId, ExecutionError> {
        if let Some(index) = self
            .loaded_code
            .iter()
            .position(|loaded| loaded.module.ptr_eq(module))
        {
            return CodeId::from_index(index)
                .ok_or(ExecutionError::LoadedModuleLimit { limit: u32::MAX });
        }
        if self.loaded_code.len() >= self.realm.limits.max_loaded_modules as usize {
            return Err(ExecutionError::LoadedModuleLimit {
                limit: self.realm.limits.max_loaded_modules,
            });
        }
        self.loaded_code
            .try_reserve_exact(1)
            .map_err(|_| ExecutionError::LoadedCodeAllocationFailed)?;
        let mut scope_resolutions = Vec::new();
        scope_resolutions
            .try_reserve_exact(module.scope_names().len())
            .map_err(|_| ExecutionError::ScopeNameAllocationFailed)?;
        let checkpoint = self.atoms.checkpoint();
        for name in module.scope_names() {
            let string = match JsString::try_from_str(name) {
                Ok(string) => string,
                Err(error) => {
                    self.atoms.rollback(checkpoint);
                    return Err(ExecutionError::ScopeNameString(error));
                }
            };
            match self.atoms.try_intern(string) {
                Ok(atom) => scope_resolutions.push(ScopeResolution {
                    atom,
                    global_slot: self.realm.resolve(atom),
                }),
                Err(error) => {
                    self.atoms.rollback(checkpoint);
                    return Err(ExecutionError::ScopeNameAtom(error));
                }
            }
        }
        let mut constant_values = Vec::new();
        if constant_values
            .try_reserve_exact(module.constants().len())
            .is_err()
        {
            self.atoms.rollback(checkpoint);
            return Err(ExecutionError::ConstantValueAllocationFailed);
        }
        for constant in module.constants() {
            let value = match constant {
                BytecodeConstant::String(code_units) => {
                    let string = match JsString::try_from_utf16(code_units) {
                        Ok(string) => string,
                        Err(error) => {
                            self.atoms.rollback(checkpoint);
                            return Err(ExecutionError::ConstantString(error));
                        }
                    };
                    let mut roots = CodeLoadRoots {
                        vm: VmRoots {
                            fiber: &mut self.fiber,
                            finalization_jobs: &mut self.finalization_jobs,
                            realm: &mut self.realm,
                            loaded_code: &mut self.loaded_code,
                        },
                        constant_values: &mut constant_values,
                    };
                    match self.heap.try_allocate_external_with_gc(
                        self.types.string,
                        0,
                        string,
                        AllocationSpace::Young,
                        &mut roots,
                    ) {
                        Ok(reference) => Some(Value::from_heap_ref(reference.raw())),
                        Err(error) => {
                            self.atoms.rollback(checkpoint);
                            return Err(ExecutionError::HeapAllocation(error));
                        }
                    }
                }
                _ => None,
            };
            constant_values.push(value);
        }
        let code = CodeId::from_index(self.loaded_code.len())
            .ok_or(ExecutionError::LoadedModuleLimit { limit: u32::MAX })?;
        self.loaded_code.push(LoadedCode {
            module: module.clone(),
            scope_resolutions: scope_resolutions.into_boxed_slice(),
            constant_values: constant_values.into_boxed_slice(),
        });
        Ok(code)
    }

    /// Executes already-loaded code without repeating module identity or scope-name resolution.
    pub fn execute_loaded(
        &mut self,
        code: CodeId,
        budget: ExecutionBudget,
    ) -> Result<RunOutcome, ExecutionError> {
        self.execute_loaded_with_batch::<{ tuning::dispatch::DEFAULT_DISPATCH_BATCH }>(code, budget)
    }

    #[inline(always)]
    fn loaded_code(&self, code: CodeId) -> Result<&LoadedCode, ExecutionError> {
        self.loaded_code
            .get(code.index())
            .ok_or(ExecutionError::InvalidCode(code))
    }

    /// Resolves an immutable scope atom once, then retains its stable global slot in loaded code.
    #[inline(always)]
    fn scope_resolution(
        &mut self,
        code: CodeId,
        scope_name: u32,
    ) -> Result<ScopeResolution, ExecutionError> {
        let resolution = self
            .loaded_code(code)?
            .scope_resolutions
            .get(scope_name as usize)
            .copied()
            .ok_or(ExecutionError::InvalidScopeName { code, scope_name })?;
        if resolution.global_slot.is_some() {
            return Ok(resolution);
        }
        let Some(global_slot) = self.realm.resolve(resolution.atom) else {
            return Ok(resolution);
        };
        let resolved = ScopeResolution {
            global_slot: Some(global_slot),
            ..resolution
        };
        self.loaded_code
            .get_mut(code.index())
            .expect("validated loaded code remains present")
            .scope_resolutions[scope_name as usize] = resolved;
        Ok(resolved)
    }

    #[inline(always)]
    fn scope_atom(&self, code: CodeId, scope_name: u32) -> Result<AtomId, ExecutionError> {
        self.loaded_code(code)?
            .scope_resolutions
            .get(scope_name as usize)
            .map(|resolution| resolution.atom)
            .ok_or(ExecutionError::InvalidScopeName { code, scope_name })
    }

    /// Converts the current integer PropertyKey subset through a bounded stack formatting buffer.
    #[cold]
    fn property_key_atom(&mut self, value: Value) -> Result<AtomId, ExecutionError> {
        let integer = value
            .as_i32()
            .ok_or(ExecutionError::UnsupportedPropertyKey(value))?;
        let key = Int32PropertyKey::new(integer);
        if let Some(atom) = self.atoms.find_latin1(key.as_bytes()) {
            return Ok(atom);
        }
        let string =
            JsString::try_from_latin1(key.as_bytes()).map_err(ExecutionError::PropertyKeyString)?;
        self.atoms
            .try_intern(string)
            .map_err(ExecutionError::PropertyKeyAtom)
    }

    #[inline(always)]
    fn typeof_value(&self, value: Value) -> Result<Value, ExecutionError> {
        let strings = self.realm.typeof_strings;
        if value.as_i32().is_some() || value.as_f64().is_some() {
            return Ok(strings.number);
        }
        if let Some(immediate) = value.as_immediate() {
            return match immediate {
                Immediate::Undefined => Ok(strings.undefined),
                Immediate::Null => Ok(strings.object),
                Immediate::False | Immediate::True => Ok(strings.boolean),
                Immediate::Hole | Immediate::Uninitialized => {
                    Err(ExecutionError::UnsupportedTypeof(value))
                }
            };
        }
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::UnsupportedTypeof(value))?;
        if self.heap.checked_reference(raw, self.types.string).is_ok() {
            return Ok(strings.string);
        }
        if self
            .heap
            .checked_reference(raw, self.types.function)
            .is_ok()
        {
            return Ok(strings.function);
        }
        if self
            .heap
            .checked_reference(raw, self.types.ordinary_object)
            .is_ok()
        {
            return Ok(strings.object);
        }
        Err(ExecutionError::UnsupportedTypeof(value))
    }

    /// Applies strict equality without allocating while preserving numeric and string semantics.
    fn strict_equal_values(&mut self, left: Value, right: Value) -> Result<bool, ExecutionError> {
        match (numeric_value(left), numeric_value(right)) {
            (Some(left), Some(right)) => return Ok(left == right),
            (Some(_), None) | (None, Some(_)) => return Ok(false),
            (None, None) => {}
        }
        if left == right {
            return Ok(true);
        }
        let (Some(left), Some(right)) = (left.as_heap_ref(), right.as_heap_ref()) else {
            return Ok(false);
        };
        let Ok(left) = self.heap.checked_reference(left, self.types.string) else {
            return Ok(false);
        };
        let Ok(right) = self.heap.checked_reference(right, self.types.string) else {
            return Ok(false);
        };
        self.heap.with_running_scope(|scope| {
            let left = scope.root(left).map_err(ExecutionError::Root)?;
            let right = scope.root(right).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let left = no_gc
                    .borrow(left, self.types.string)
                    .map_err(ExecutionError::NoGcBorrow)?;
                let right = no_gc
                    .borrow(right, self.types.string)
                    .map_err(ExecutionError::NoGcBorrow)?;
                Ok(left == right)
            })
        })
    }

    #[inline(always)]
    fn is_truthy_value(&mut self, value: Value) -> Result<bool, ExecutionError> {
        if let Some(raw) = value.as_heap_ref()
            && let Ok(string) = self.heap.checked_reference(raw, self.types.string)
        {
            return self.heap.with_running_scope(|scope| {
                let string = scope.root(string).map_err(ExecutionError::Root)?;
                scope.with_no_gc_scope(|no_gc| {
                    no_gc
                        .borrow(string, self.types.string)
                        .map(|string| !string.is_empty())
                        .map_err(ExecutionError::NoGcBorrow)
                })
            });
        }
        Ok(is_non_string_truthy(value))
    }

    /// Executes with a fixed internal batch size so each monomorphization preserves the same fuel contract.
    #[cfg(test)]
    fn execute_with_batch<const N: usize>(
        &mut self,
        module: &CompiledModule,
        budget: ExecutionBudget,
    ) -> Result<RunOutcome, ExecutionError> {
        let code = self.load_module(module)?;
        self.execute_loaded_with_batch::<N>(code, budget)
    }

    /// Runs one resolved code entry with a test-selectable dispatch batch monomorphization.
    fn execute_loaded_with_batch<const N: usize>(
        &mut self,
        code: CodeId,
        mut budget: ExecutionBudget,
    ) -> Result<RunOutcome, ExecutionError> {
        if N == 0 {
            return Err(ExecutionError::InvalidDispatchBatch { batch: N });
        }
        let entry_function = self.loaded_code(code)?.module.entry_function();
        self.enter(code, entry_function)?;
        loop {
            if budget.fuel == 0 || budget.quantum == 0 {
                return Ok(RunOutcome::BudgetExhausted);
            }
            if let Some(outcome) = self.execute_batch::<N>(&mut budget)? {
                return Ok(outcome);
            }
        }
    }

    /// The `N` parameter is an internal dispatch-tuning knob; every executed opcode still consumes exact fuel.
    #[inline(always)]
    fn execute_batch<const N: usize>(
        &mut self,
        budget: &mut ExecutionBudget,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        for _ in 0..N {
            if budget.fuel == 0 || budget.quantum == 0 {
                return Ok(Some(RunOutcome::BudgetExhausted));
            }
            let frame = *self
                .fiber
                .frames
                .last()
                .expect("entry frame exists while executing");
            let function = self
                .loaded_code(frame.code)?
                .module
                .function(frame.function)
                .ok_or(ExecutionError::MissingEntryFunction(frame.function))?;
            let instruction = decode_instruction(function.bytecode().bytecode().words(), frame.pc)
                .map_err(|_| ExecutionError::DecodeInvariant(frame.pc))?;
            let next_pc = WordOffset::new(frame.pc.index() + u32::from(instruction.word_len));
            self.fiber
                .frames
                .last_mut()
                .expect("frame remains active")
                .pc = next_pc;
            budget.fuel -= 1;
            budget.quantum -= 1;
            if let Some(outcome) = self.dispatch(
                frame.code,
                frame.pc,
                instruction.opcode,
                instruction.operands,
                frame.base,
            )? {
                return Ok(Some(outcome));
            }
        }
        Ok(None)
    }

    /// Implements the arithmetic subset emitted by the current compiler; later opcodes extend this match.
    #[inline(always)]
    fn dispatch(
        &mut self,
        code: CodeId,
        instruction_offset: WordOffset,
        opcode: Opcode,
        operands: [u32; 3],
        base: u32,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        match opcode {
            Opcode::Nop => {}
            Opcode::LoadUndefined => self.write(
                base,
                operands[0],
                Value::from_immediate(Immediate::Undefined),
            )?,
            Opcode::LoadNull => {
                self.write(base, operands[0], Value::from_immediate(Immediate::Null))?
            }
            Opcode::LoadFalse => {
                self.write(base, operands[0], Value::from_immediate(Immediate::False))?
            }
            Opcode::LoadTrue => {
                self.write(base, operands[0], Value::from_immediate(Immediate::True))?
            }
            Opcode::LoadImmediate => {
                self.write(base, operands[0], Value::from_i32(operands[1] as i32))?
            }
            Opcode::LoadConstant => {
                let constant_index = operands[1] as usize;
                let loaded = self.loaded_code(code)?;
                let constant = loaded
                    .module
                    .constants()
                    .get(constant_index)
                    .ok_or(ExecutionError::UnsupportedConstant(operands[1]))?;
                let value = match constant {
                    BytecodeConstant::NumberBits(bits) => Value::from_f64(f64::from_bits(*bits)),
                    BytecodeConstant::String(_) => loaded
                        .constant_values
                        .get(constant_index)
                        .copied()
                        .flatten()
                        .ok_or(ExecutionError::UnsupportedConstant(operands[1]))?,
                    _ => return Err(ExecutionError::UnsupportedConstant(operands[1])),
                };
                self.write(base, operands[0], value)?;
            }
            Opcode::Move => {
                let value = self.read(base, operands[1])?;
                self.write(base, operands[0], value)?;
            }
            Opcode::Not => {
                let value = if self.is_truthy_value(self.read(base, operands[1])?)? {
                    Value::from_immediate(Immediate::False)
                } else {
                    Value::from_immediate(Immediate::True)
                };
                self.write(base, operands[0], value)?;
            }
            Opcode::Negate => {
                let value = numeric_negate(self.read(base, operands[1])?);
                self.write(base, operands[0], value)?;
            }
            Opcode::Add | Opcode::Sub | Opcode::Mul | Opcode::Div => {
                let left = self.read(base, operands[1])?;
                let right = self.read(base, operands[2])?;
                self.write(base, operands[0], numeric_binary(opcode, left, right))?;
            }
            Opcode::StrictEqual => {
                let left = self.read(base, operands[1])?;
                let right = self.read(base, operands[2])?;
                let value = if self.strict_equal_values(left, right)? {
                    Value::from_immediate(Immediate::True)
                } else {
                    Value::from_immediate(Immediate::False)
                };
                self.write(base, operands[0], value)?;
            }
            Opcode::Typeof => {
                let value = self.typeof_value(self.read(base, operands[1])?)?;
                self.write(base, operands[0], value)?;
            }
            Opcode::LessThan => {
                let left = self.read(base, operands[1])?;
                let right = self.read(base, operands[2])?;
                let value = if numeric_less_than(left, right) {
                    Value::from_immediate(Immediate::True)
                } else {
                    Value::from_immediate(Immediate::False)
                };
                self.write(base, operands[0], value)?;
            }
            Opcode::InstanceOf => {
                let left = self.read(base, operands[1])?;
                let right = self.read(base, operands[2])?;
                let value = if self.ordinary_instance_of(left, right)? {
                    Value::from_immediate(Immediate::True)
                } else {
                    Value::from_immediate(Immediate::False)
                };
                self.write(base, operands[0], value)?;
            }
            Opcode::Jump => self.set_pc(WordOffset::new(operands[0])),
            Opcode::JumpIfFalse => {
                if !self.is_truthy_value(self.read(base, operands[0])?)? {
                    self.set_pc(WordOffset::new(operands[1]));
                }
            }
            Opcode::JumpIfTrue => {
                if self.is_truthy_value(self.read(base, operands[0])?)? {
                    self.set_pc(WordOffset::new(operands[1]));
                }
            }
            Opcode::JumpIfNotNullish => {
                if !is_nullish(self.read(base, operands[0])?) {
                    self.set_pc(WordOffset::new(operands[1]));
                }
            }
            Opcode::LoadScope => {
                let resolution = self.scope_resolution(code, operands[1])?;
                let value = self
                    .scope_value(resolution)
                    .ok_or(ExecutionError::UnresolvedBinding(resolution.atom))?;
                self.write(base, operands[0], value)?;
            }
            Opcode::StoreScope => {
                let value = self.read(base, operands[0])?;
                self.store_scope(code, operands[1], value)?;
            }
            Opcode::StoreResolvedScope => {
                let value = self.read(base, operands[0])?;
                self.store_resolved_scope(code, operands[1], value)?;
            }
            Opcode::DeclareScope => {
                self.declare_scope(code, operands[0])?;
            }
            Opcode::CreateClosure => {
                self.create_closure(code, base, operands[0], FunctionId::new(operands[1]))?
            }
            Opcode::CreateObject => {
                let object = self.create_ordinary_object()?;
                self.write(base, operands[0], object)?;
            }
            Opcode::LoadException => {
                let value = self
                    .fiber
                    .pending_exception
                    .take()
                    .ok_or(ExecutionError::MissingPendingException)?;
                self.write(base, operands[0], value)?;
            }
            Opcode::LoadThis => {
                let value = self
                    .fiber
                    .frames
                    .last()
                    .expect("this load always has an active frame")
                    .this_value;
                self.write(base, operands[0], value)?;
            }
            Opcode::LoadNewTarget => {
                let value = self
                    .fiber
                    .frames
                    .last()
                    .expect("new.target load always has an active frame")
                    .new_target;
                self.write(base, operands[0], value)?;
            }
            Opcode::GetById => {
                let receiver = self.read(base, operands[1])?;
                let key = self.scope_atom(code, operands[2])?;
                let value = self
                    .get_data_property(receiver, key)?
                    .unwrap_or(Value::from_immediate(Immediate::Undefined));
                self.write(base, operands[0], value)?;
            }
            Opcode::SetById => {
                let receiver = self.read(base, operands[0])?;
                let value = self.read(base, operands[1])?;
                let key = self.scope_atom(code, operands[2])?;
                self.set_own_data_property(receiver, key, value)?;
            }
            Opcode::GetByValue => {
                let receiver = self.read(base, operands[1])?;
                let key = self.property_key_atom(self.read(base, operands[2])?)?;
                let value = self
                    .get_data_property(receiver, key)?
                    .unwrap_or(Value::from_immediate(Immediate::Undefined));
                self.write(base, operands[0], value)?;
            }
            Opcode::SetByValue => {
                let receiver = self.read(base, operands[0])?;
                let value = self.read(base, operands[1])?;
                let key = self.property_key_atom(self.read(base, operands[2])?)?;
                self.set_own_data_property(receiver, key, value)?;
            }
            Opcode::Call => self.call(CallSite {
                caller_base: base,
                destination: operands[0],
                callee_register: operands[1],
                argument_count: operands[2],
                this_value: Value::from_immediate(Immediate::Undefined),
                new_target: Value::from_immediate(Immediate::Undefined),
                construct_receiver: None,
                call_site: instruction_offset,
            })?,
            Opcode::CallWithReceiver => {
                let receiver = self.read(base, operands[1])?;
                self.call(CallSite {
                    caller_base: base,
                    destination: operands[0],
                    callee_register: operands[1] + 1,
                    argument_count: operands[2],
                    this_value: receiver,
                    new_target: Value::from_immediate(Immediate::Undefined),
                    construct_receiver: None,
                    call_site: instruction_offset,
                })?;
            }
            Opcode::Construct => self.construct(
                base,
                operands[0],
                operands[1],
                operands[2],
                instruction_offset,
            )?,
            Opcode::Return => {
                let value = self.read(base, operands[0])?;
                if self.fiber.frames.len() == 1 {
                    return Ok(Some(RunOutcome::Completed(value)));
                }
                return self.return_from_callee(value);
            }
            Opcode::Throw => {
                let value = self.read(base, operands[0])?;
                return self.throw_value(value, instruction_offset);
            }
            _ => return Err(ExecutionError::UnsupportedOpcode(opcode)),
        }
        Ok(None)
    }

    /// Resolves non-writable primitive realm properties before mutable global bindings.
    #[inline(always)]
    fn intrinsic_scope_value(&self, name: AtomId) -> Option<Value> {
        let string = self.atoms.get(name)?;
        match string.len() {
            3 if string.equals_latin1(b"NaN") => Some(Value::from_f64(f64::NAN)),
            8 if string.equals_latin1(b"Infinity") => Some(Value::from_f64(f64::INFINITY)),
            9 if string.equals_latin1(b"undefined") => {
                Some(Value::from_immediate(Immediate::Undefined))
            }
            _ => None,
        }
    }

    #[inline(always)]
    fn scope_value(&self, resolution: ScopeResolution) -> Option<Value> {
        self.intrinsic_scope_value(resolution.atom).or_else(|| {
            resolution
                .global_slot
                .and_then(|slot| self.realm.get_slot(slot))
        })
    }

    /// Writes through a cached global slot or publishes the binding once on the cold path.
    #[inline(always)]
    fn store_scope(
        &mut self,
        code: CodeId,
        scope_name: u32,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let resolution = self.scope_resolution(code, scope_name)?;
        if let Some(slot) = resolution.global_slot {
            self.realm.set_slot(slot, value);
            return Ok(());
        }
        self.realm.set(resolution.atom, value)
    }

    #[inline(always)]
    fn declare_scope(&mut self, code: CodeId, scope_name: u32) -> Result<(), ExecutionError> {
        let resolution = self.scope_resolution(code, scope_name)?;
        self.declare_scope_resolution(resolution)
    }

    #[inline(always)]
    fn declare_scope_resolution(
        &mut self,
        resolution: ScopeResolution,
    ) -> Result<(), ExecutionError> {
        if self.scope_value(resolution).is_some() {
            return Ok(());
        }
        self.realm
            .set(resolution.atom, Value::from_immediate(Immediate::Undefined))
    }

    /// Updates a mutable global or applies the strict/sloppy primitive-intrinsic write contract.
    fn store_resolved_scope(
        &mut self,
        code: CodeId,
        scope_name: u32,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let resolution = self.scope_resolution(code, scope_name)?;
        if let Some(slot) = resolution.global_slot {
            self.realm.set_slot(slot, value);
            return Ok(());
        }
        if self.intrinsic_scope_value(resolution.atom).is_some() {
            let strict = self
                .fiber
                .frames
                .last()
                .is_some_and(|frame| frame.strictness == Strictness::Strict);
            return if strict {
                Err(ExecutionError::ReadOnlyBinding(resolution.atom))
            } else {
                Ok(())
            };
        }
        Err(ExecutionError::UnresolvedBinding(resolution.atom))
    }

    fn enter(&mut self, code: CodeId, function_id: FunctionId) -> Result<(), ExecutionError> {
        let (layout, kind) = {
            let function = self
                .loaded_code(code)?
                .module
                .function(function_id)
                .ok_or(ExecutionError::MissingEntryFunction(function_id))?;
            (function.layout(), function.kind())
        };
        let register_count = usize::try_from(layout.register_count)
            .map_err(|_| ExecutionError::RegisterWindowTooLarge(layout.register_count))?;
        if layout.register_count > self.stack_limits.max_registers {
            return Err(ExecutionError::RegisterStackLimit {
                limit: self.stack_limits.max_registers,
                requested: layout.register_count,
            });
        }
        if self.stack_limits.max_frames == 0 {
            return Err(ExecutionError::CallStackLimit { limit: 0 });
        }
        self.fiber.frames.clear();
        self.fiber.registers.clear();
        self.fiber.handlers.clear();
        self.fiber.completions.clear();
        self.fiber.pending_exception = None;
        self.reserve_entry_state(layout, register_count)?;
        self.fiber
            .registers
            .resize(register_count, Value::from_immediate(Immediate::Undefined));
        self.fiber.frames.push(Frame {
            code,
            function: function_id,
            pc: WordOffset::new(0),
            base: 0,
            environment: None,
            return_register: None,
            this_value: Value::from_immediate(Immediate::Undefined),
            new_target: Value::from_immediate(Immediate::Undefined),
            strictness: strictness_for(kind),
            argument_base: 0,
            argument_count: 0,
            handler_base: 0,
            completion_base: 0,
            construct_receiver: None,
            call_site: None,
        });
        Ok(())
    }

    /// Allocates a real GC-managed callable instead of encoding FunctionId in a reserved Value tag.
    #[inline(never)]
    fn create_closure(
        &mut self,
        code: CodeId,
        base: u32,
        destination: u32,
        function: FunctionId,
    ) -> Result<(), ExecutionError> {
        self.loaded_code(code)?
            .module
            .function(function)
            .ok_or(ExecutionError::MissingEntryFunction(function))?;
        let environment = self.fiber.frames.last().and_then(|frame| frame.environment);
        let roots = &mut VmRoots {
            fiber: &mut self.fiber,
            finalization_jobs: &mut self.finalization_jobs,
            realm: &mut self.realm,
            loaded_code: &mut self.loaded_code,
        };
        let closure = self
            .heap
            .try_allocate_with_gc(
                self.types.function,
                0,
                0,
                FunctionObject {
                    code,
                    function,
                    environment,
                    function_prototype: None,
                    ordinary: OrdinaryObject {
                        shape: ShapeId::EMPTY,
                        storage: None,
                        prototype: Value::from_immediate(Immediate::Null),
                    },
                },
                AllocationSpace::Young,
                roots,
            )
            .map_err(ExecutionError::HeapAllocation)?;
        self.write(base, destination, Value::from_heap_ref(closure.raw()))
    }

    /// Validates the constructor before allocation, creates its receiver, and pushes one JS frame.
    #[inline(never)]
    fn construct(
        &mut self,
        caller_base: u32,
        destination: u32,
        callee_register: u32,
        argument_count: u32,
        call_site: WordOffset,
    ) -> Result<(), ExecutionError> {
        let constructor = self.read(caller_base, callee_register)?;
        let raw = constructor
            .as_heap_ref()
            .ok_or(ExecutionError::NonConstructor(constructor))?;
        self.heap
            .checked_reference(raw, self.types.function)
            .map_err(|_| ExecutionError::NonConstructor(constructor))?;
        let prototype_atom = self.prototype_atom()?;
        let prototype = self
            .get_data_property(constructor, prototype_atom)?
            .filter(|value| self.is_object_value(*value))
            .unwrap_or(Value::from_immediate(Immediate::Null));
        let receiver = self.create_ordinary_object_with_prototype(prototype)?;
        self.call(CallSite {
            caller_base,
            destination,
            callee_register,
            argument_count,
            this_value: receiver,
            new_target: constructor,
            construct_receiver: Some(receiver),
            call_site,
        })
    }

    /// Resolves one callable and pushes its exact register/frame window without Rust recursion.
    #[inline(never)]
    fn call(&mut self, site: CallSite) -> Result<(), ExecutionError> {
        let callee = self.read(site.caller_base, site.callee_register)?;
        let raw = callee
            .as_heap_ref()
            .ok_or(ExecutionError::NonCallable(callee))?;
        let reference = self
            .heap
            .checked_reference(raw, self.types.function)
            .map_err(ExecutionError::HeapReference)?;
        let function_object = self.heap.with_running_scope(|scope| {
            let local = scope.root(reference).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(local, self.types.function)
                    .copied()
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })?;
        let (layout, kind) = {
            let function = self
                .loaded_code(function_object.code)?
                .module
                .function(function_object.function)
                .ok_or(ExecutionError::MissingEntryFunction(
                    function_object.function,
                ))?;
            (function.layout(), function.kind())
        };
        self.push_call_frame(
            ResolvedCallTarget {
                object: function_object,
                layout,
                kind,
            },
            site,
        )
    }

    /// Reserves the callee state before mutation, then copies the supplied positional arguments.
    fn push_call_frame(
        &mut self,
        target: ResolvedCallTarget,
        site: CallSite,
    ) -> Result<(), ExecutionError> {
        if self.fiber.frames.len() >= self.stack_limits.max_frames as usize {
            return Err(ExecutionError::CallStackLimit {
                limit: self.stack_limits.max_frames,
            });
        }
        let register_count = target.layout.register_count;
        let callee_base = u32::try_from(self.fiber.registers.len())
            .map_err(|_| ExecutionError::RegisterWindowTooLarge(register_count))?;
        let requested = callee_base
            .checked_add(register_count)
            .ok_or(ExecutionError::RegisterWindowTooLarge(register_count))?;
        if requested > self.stack_limits.max_registers {
            return Err(ExecutionError::RegisterStackLimit {
                limit: self.stack_limits.max_registers,
                requested,
            });
        }
        let argument_base = site
            .caller_base
            .checked_add(site.callee_register)
            .and_then(|base| base.checked_add(1))
            .ok_or(ExecutionError::RegisterWindowTooLarge(register_count))?;
        let additional = register_count as usize;
        self.fiber
            .frames
            .try_reserve_exact(1)
            .map_err(|_| ExecutionError::FrameAllocationFailed)?;
        self.fiber
            .registers
            .try_reserve_exact(additional)
            .map_err(|_| ExecutionError::RegisterAllocationFailed)?;
        self.fiber.registers.resize(
            requested as usize,
            Value::from_immediate(Immediate::Undefined),
        );
        let copied_arguments = site.argument_count.min(target.layout.argument_count);
        for index in 0..copied_arguments {
            let value = self
                .fiber
                .registers
                .get((argument_base + index) as usize)
                .copied()
                .ok_or(ExecutionError::InvalidRegister(RegisterId::new(
                    site.callee_register + 1 + index,
                )))?;
            self.write(callee_base, index, value)?;
        }
        self.fiber.frames.push(Frame {
            code: target.object.code,
            function: target.object.function,
            pc: WordOffset::new(0),
            base: callee_base,
            environment: target.object.environment,
            return_register: Some(RegisterId::new(site.destination)),
            this_value: site.this_value,
            new_target: site.new_target,
            construct_receiver: site.construct_receiver,
            strictness: strictness_for(target.kind),
            argument_base,
            argument_count: site.argument_count,
            handler_base: self.fiber.handlers.len() as u32,
            completion_base: self.fiber.completions.len() as u32,
            call_site: Some(site.call_site),
        });
        Ok(())
    }

    /// Pops a non-entry frame and restores the caller checkpoints outside the top-level Return path.
    #[cold]
    #[inline(never)]
    fn return_from_callee(&mut self, value: Value) -> Result<Option<RunOutcome>, ExecutionError> {
        let frame = self
            .fiber
            .frames
            .pop()
            .expect("callee return always has an active frame");
        let value = match frame.construct_receiver {
            Some(receiver) if !self.is_object_value(value) => receiver,
            _ => value,
        };
        self.fiber.registers.truncate(frame.base as usize);
        self.fiber.handlers.truncate(frame.handler_base as usize);
        self.fiber
            .completions
            .truncate(frame.completion_base as usize);
        let destination = frame
            .return_register
            .expect("non-entry frames always retain a caller destination");
        let caller_base = self
            .fiber
            .frames
            .last()
            .expect("a callee return with a destination retains its caller")
            .base;
        self.write(caller_base, destination.index(), value)?;
        Ok(None)
    }

    /// Propagates a thrown value through explicit frames until an immutable handler range matches.
    #[cold]
    #[inline(never)]
    fn throw_value(
        &mut self,
        value: Value,
        mut instruction_offset: WordOffset,
    ) -> Result<Option<RunOutcome>, ExecutionError> {
        loop {
            let frame = *self
                .fiber
                .frames
                .last()
                .expect("throw dispatch always has an active frame");
            if let Some(handler) = self.find_exception_handler(frame, instruction_offset)? {
                if handler.kind != HandlerKind::Catch {
                    return Err(ExecutionError::UnsupportedExceptionHandler(handler.kind));
                }
                let active = self
                    .fiber
                    .frames
                    .last_mut()
                    .expect("matched handler retains its frame");
                active.pc = handler.handler;
                self.fiber.handlers.truncate(frame.handler_base as usize);
                self.fiber
                    .completions
                    .truncate(frame.completion_base as usize);
                self.fiber.pending_exception = Some(value);
                return Ok(None);
            }
            if self.fiber.frames.len() == 1 {
                return Ok(self.unhandled_throw(value));
            }
            let frame = self
                .fiber
                .frames
                .pop()
                .expect("non-entry throw retains a callee frame");
            self.fiber.registers.truncate(frame.base as usize);
            self.fiber.handlers.truncate(frame.handler_base as usize);
            self.fiber
                .completions
                .truncate(frame.completion_base as usize);
            instruction_offset = frame
                .call_site
                .expect("every non-entry frame records its caller call-site");
        }
    }

    /// Selects the innermost half-open handler range for one verified function offset.
    #[inline]
    fn find_exception_handler(
        &self,
        frame: Frame,
        instruction_offset: WordOffset,
    ) -> Result<Option<HandlerEntry>, ExecutionError> {
        let function = self
            .loaded_code(frame.code)?
            .module
            .function(frame.function)
            .ok_or(ExecutionError::MissingEntryFunction(frame.function))?;
        Ok(function.handlers().iter().rev().copied().find(|handler| {
            handler.protected_start.index() <= instruction_offset.index()
                && instruction_offset.index() < handler.protected_end.index()
        }))
    }

    /// Preserves the active fiber as the root owner until the host observes the unhandled value.
    #[cold]
    #[inline(never)]
    fn unhandled_throw(&mut self, value: Value) -> Option<RunOutcome> {
        Some(RunOutcome::Thrown(value))
    }

    /// Reserves the exact entry-function execution windows before any opcode can push into them.
    ///
    /// Calls will extend these windows from the callee's verified metadata in a later VM package.
    /// Until then, reserving the handler and completion depths here proves the no-reallocation
    /// contract without conflating bytecode decoding with collection growth policy.
    fn reserve_entry_state(
        &mut self,
        layout: tachyon_bytecode::FunctionLayout,
        register_count: usize,
    ) -> Result<(), ExecutionError> {
        let handler_depth = usize::try_from(layout.max_handler_depth)
            .map_err(|_| ExecutionError::HandlerStackTooLarge(layout.max_handler_depth))?;
        let completion_depth = usize::try_from(layout.max_completion_depth)
            .map_err(|_| ExecutionError::CompletionStackTooLarge(layout.max_completion_depth))?;
        self.fiber
            .frames
            .try_reserve_exact(1)
            .map_err(|_| ExecutionError::FrameAllocationFailed)?;
        self.fiber
            .registers
            .try_reserve_exact(register_count)
            .map_err(|_| ExecutionError::RegisterAllocationFailed)?;
        self.fiber
            .handlers
            .try_reserve_exact(handler_depth)
            .map_err(|_| ExecutionError::HandlerAllocationFailed)?;
        self.fiber
            .completions
            .try_reserve_exact(completion_depth)
            .map_err(|_| ExecutionError::CompletionAllocationFailed)?;
        Ok(())
    }

    #[inline(always)]
    fn read(&self, base: u32, register: u32) -> Result<Value, ExecutionError> {
        self.fiber
            .registers
            .get(base as usize + register as usize)
            .copied()
            .ok_or(ExecutionError::InvalidRegister(RegisterId::new(register)))
    }

    #[inline(always)]
    fn write(&mut self, base: u32, register: u32, value: Value) -> Result<(), ExecutionError> {
        let slot = self
            .fiber
            .registers
            .get_mut(base as usize + register as usize)
            .ok_or(ExecutionError::InvalidRegister(RegisterId::new(register)))?;
        *slot = value;
        Ok(())
    }

    #[inline(always)]
    fn set_pc(&mut self, pc: WordOffset) {
        self.fiber
            .frames
            .last_mut()
            .expect("frame remains active while jumping")
            .pc = pc;
    }
}

impl Trace for Isolate {
    #[inline]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.trace_roots(tracer);
    }
}

/// Modules are intrinsically strict; other function kinds await lowering-provided strict metadata.
#[inline]
fn strictness_for(kind: FunctionKind) -> Strictness {
    if matches!(kind, FunctionKind::Module) {
        Strictness::Strict
    } else {
        Strictness::Sloppy
    }
}

struct Int32PropertyKey {
    bytes: [u8; 11],
    start: u8,
}

impl Int32PropertyKey {
    /// Formats every i32 PropertyKey into owned stack storage without Rust formatting machinery.
    fn new(value: i32) -> Self {
        let mut bytes = [0_u8; 11];
        let mut start = bytes.len();
        let mut magnitude = value.unsigned_abs();
        loop {
            start -= 1;
            bytes[start] = b'0' + (magnitude % 10) as u8;
            magnitude /= 10;
            if magnitude == 0 {
                break;
            }
        }
        if value < 0 {
            start -= 1;
            bytes[start] = b'-';
        }
        Self {
            bytes,
            start: start as u8,
        }
    }

    #[inline(always)]
    fn as_bytes(&self) -> &[u8] {
        &self.bytes[self.start as usize..]
    }
}

#[inline(always)]
fn numeric_binary(opcode: Opcode, left: Value, right: Value) -> Value {
    if let (Some(left), Some(right)) = (left.as_i32(), right.as_i32()) {
        let integer = match opcode {
            Opcode::Add => left.checked_add(right),
            Opcode::Sub => left.checked_sub(right),
            Opcode::Mul => left.checked_mul(right),
            Opcode::Div if right != 0 && left % right == 0 => left.checked_div(right),
            _ => None,
        };
        if let Some(integer) = integer {
            return Value::from_i32(integer);
        }
    }
    let left_number = left
        .as_i32()
        .map_or_else(|| left.as_f64().unwrap_or(f64::NAN), f64::from);
    let right_number = right
        .as_i32()
        .map_or_else(|| right.as_f64().unwrap_or(f64::NAN), f64::from);
    Value::from_f64(match opcode {
        Opcode::Add => left_number + right_number,
        Opcode::Sub => left_number - right_number,
        Opcode::Mul => left_number * right_number,
        Opcode::Div => left_number / right_number,
        _ => unreachable!("numeric binary dispatch only supplies arithmetic opcodes"),
    })
}

#[inline(always)]
fn numeric_negate(value: Value) -> Value {
    if let Some(integer) = value.as_i32() {
        if integer == 0 {
            return Value::from_f64(-0.0);
        }
        return integer
            .checked_neg()
            .map_or_else(|| Value::from_f64(-f64::from(integer)), Value::from_i32);
    }
    if let Some(number) = value.as_f64() {
        return Value::from_f64(-number);
    }
    Value::from_f64(match value.as_immediate() {
        Some(Immediate::Null | Immediate::False) => -0.0,
        Some(Immediate::True) => -1.0,
        _ => f64::NAN,
    })
}

#[inline(always)]
fn numeric_value(value: Value) -> Option<f64> {
    value.as_i32().map(f64::from).or_else(|| value.as_f64())
}

#[inline(always)]
fn numeric_less_than(left: Value, right: Value) -> bool {
    match (left.as_i32(), right.as_i32()) {
        (Some(left), Some(right)) => left < right,
        _ => match (numeric_value(left), numeric_value(right)) {
            (Some(left), Some(right)) => left < right,
            _ => false,
        },
    }
}

#[inline(always)]
fn is_non_string_truthy(value: Value) -> bool {
    if let Some(integer) = value.as_i32() {
        return integer != 0;
    }
    if let Some(number) = value.as_f64() {
        return number != 0.0 && !number.is_nan();
    }
    !matches!(
        value.as_immediate(),
        Some(Immediate::Undefined | Immediate::Null | Immediate::False)
    )
}

#[inline(always)]
fn is_nullish(value: Value) -> bool {
    matches!(
        value.as_immediate(),
        Some(Immediate::Undefined | Immediate::Null)
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tachyon_bytecode::{
        Bytecode, BytecodeBuilder, BytecodeConstant, CompiledFunctionTemplate, CompiledModule,
        FunctionId, FunctionKind, FunctionLayout, FunctionMetadata, SourceSpan, encode_instruction,
    };
    use tachyon_gc::{ForcedCollectionMode, GcRef, HeapLimit, SPAN_SIZE_BYTES, Tracer};
    use tachyon_value::RawHeapRef;

    use super::*;

    fn test_isolate() -> Isolate {
        Isolate::new(IsolateConfig::new(
            AtomTableConfig::new(1_024, 1024 * 1024, AtomHashSeed::new(1, 2)),
            HeapLimit::new(4 * SPAN_SIZE_BYTES),
            StackLimits::new(64, 4_096),
            RealmLimits::new(64, 1_024),
        ))
        .expect("test isolate descriptors register")
    }

    fn arithmetic_module() -> CompiledModule {
        binary_module(Opcode::Add, "1 + 2")
    }

    fn less_than_module() -> CompiledModule {
        binary_module(Opcode::LessThan, "1 < 2")
    }

    /// Compares the canonical typeof number value with an independently loaded string literal.
    fn typeof_module() -> CompiledModule {
        let span = SourceSpan { start: 0, end: 1 };
        let mut builder = BytecodeBuilder::with_capacity(5, 0);
        builder.emit(Opcode::LoadImmediate, &[0, 1], span).unwrap();
        builder.emit(Opcode::Typeof, &[1, 0], span).unwrap();
        builder.emit(Opcode::LoadConstant, &[2, 0], span).unwrap();
        builder.emit(Opcode::StrictEqual, &[3, 1, 2], span).unwrap();
        builder.emit(Opcode::Return, &[3], span).unwrap();
        single_function_module(
            "typeof number",
            vec![BytecodeConstant::string_from_utf16(
                "number".encode_utf16().collect(),
            )],
            builder,
        )
    }

    /// Loads two distinct strings so forced collection runs while the pending cache owns a root.
    fn string_constant_root_module() -> CompiledModule {
        let span = SourceSpan { start: 0, end: 1 };
        let mut builder = BytecodeBuilder::with_capacity(4, 0);
        builder.emit(Opcode::LoadConstant, &[0, 0], span).unwrap();
        builder.emit(Opcode::LoadConstant, &[1, 1], span).unwrap();
        builder.emit(Opcode::StrictEqual, &[2, 0, 1], span).unwrap();
        builder.emit(Opcode::Return, &[2], span).unwrap();
        single_function_module(
            "rooted strings",
            vec![
                BytecodeConstant::string_from_utf16("left".encode_utf16().collect()),
                BytecodeConstant::string_from_utf16("right".encode_utf16().collect()),
            ],
            builder,
        )
    }

    /// Declares one global twice around a write so dispatch tests prove redeclaration is a no-op.
    fn scoped_var_module() -> CompiledModule {
        let span = SourceSpan { start: 0, end: 1 };
        let mut builder = BytecodeBuilder::with_capacity(6, 0);
        builder.emit(Opcode::DeclareScope, &[0], span).unwrap();
        builder.emit(Opcode::LoadImmediate, &[0, 7], span).unwrap();
        builder
            .emit(Opcode::StoreResolvedScope, &[0, 0], span)
            .unwrap();
        builder.emit(Opcode::DeclareScope, &[0], span).unwrap();
        builder.emit(Opcode::LoadScope, &[1, 0], span).unwrap();
        builder.emit(Opcode::Return, &[1], span).unwrap();
        let (bytecode, source_map, register_count) = builder.finish().unwrap();
        CompiledModule::new(
            Arc::from("var answer = 7; var answer; answer;"),
            Vec::new(),
            vec![Arc::from("answer")],
            vec![CompiledFunctionTemplate::new(
                FunctionId::new(0),
                bytecode,
                FunctionMetadata {
                    layout: FunctionLayout {
                        register_count,
                        ..FunctionLayout::default()
                    },
                    source_map,
                    ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
                },
            )],
            FunctionId::new(0),
        )
        .unwrap()
    }

    /// Freezes one builder into a script module with caller-provided immutable constants.
    fn single_function_module(
        source: &'static str,
        constants: Vec<BytecodeConstant>,
        builder: BytecodeBuilder,
    ) -> CompiledModule {
        let (bytecode, source_map, register_count) = builder.finish().unwrap();
        let metadata = FunctionMetadata {
            layout: FunctionLayout {
                register_count,
                ..FunctionLayout::default()
            },
            source_map,
            ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
        };
        CompiledModule::new(
            Arc::from(source),
            constants,
            Vec::new(),
            vec![CompiledFunctionTemplate::new(
                FunctionId::new(0),
                bytecode,
                metadata,
            )],
            FunctionId::new(0),
        )
        .unwrap()
    }

    /// Builds a minimal verified binary-op fixture over the integer values one and two.
    fn binary_module(opcode: Opcode, source: &'static str) -> CompiledModule {
        let mut words = encode_instruction(Opcode::LoadImmediate, &[0, 1]).unwrap();
        words.extend(encode_instruction(Opcode::LoadImmediate, &[1, 2]).unwrap());
        words.extend(encode_instruction(opcode, &[2, 0, 1]).unwrap());
        words.extend(encode_instruction(Opcode::Return, &[2]).unwrap());
        let metadata = FunctionMetadata::new(
            FunctionKind::Script,
            FunctionLayout {
                register_count: 3,
                ..FunctionLayout::default()
            },
        );
        CompiledModule::new(
            Arc::from(source),
            Vec::new(),
            Vec::new(),
            vec![CompiledFunctionTemplate::new(
                FunctionId::new(0),
                Bytecode::from_words(words),
                metadata,
            )],
            FunctionId::new(0),
        )
        .unwrap()
    }

    /// Builds one non-capturing function call with a contiguous single-argument window.
    fn call_module() -> CompiledModule {
        let span = SourceSpan { start: 0, end: 0 };
        let mut entry = BytecodeBuilder::default();
        entry.emit(Opcode::CreateClosure, &[0, 1], span).unwrap();
        entry.emit(Opcode::LoadImmediate, &[1, 40], span).unwrap();
        entry.emit(Opcode::Call, &[2, 0, 1], span).unwrap();
        entry.emit(Opcode::Return, &[2], span).unwrap();
        let (entry_bytecode, entry_source_map, entry_registers) = entry.finish().unwrap();

        let mut callee = BytecodeBuilder::default();
        callee.emit(Opcode::LoadImmediate, &[1, 2], span).unwrap();
        callee.emit(Opcode::Add, &[2, 0, 1], span).unwrap();
        callee.emit(Opcode::Return, &[2], span).unwrap();
        let (callee_bytecode, callee_source_map, callee_registers) = callee.finish().unwrap();

        CompiledModule::new(
            Arc::from("function addTwo(value) { return value + 2; } addTwo(40);"),
            vec![],
            vec![],
            vec![
                CompiledFunctionTemplate::new(
                    FunctionId::new(0),
                    entry_bytecode,
                    FunctionMetadata {
                        kind: FunctionKind::Script,
                        layout: FunctionLayout {
                            register_count: entry_registers,
                            ..FunctionLayout::default()
                        },
                        source_map: entry_source_map,
                        handlers: Arc::default(),
                        suspend_points: Arc::default(),
                        feedback_sites: Arc::default(),
                        binding_plan: Arc::default(),
                    },
                ),
                CompiledFunctionTemplate::new(
                    FunctionId::new(1),
                    callee_bytecode,
                    FunctionMetadata {
                        kind: FunctionKind::Ordinary,
                        layout: FunctionLayout {
                            register_count: callee_registers,
                            argument_count: 1,
                            ..FunctionLayout::default()
                        },
                        source_map: callee_source_map,
                        handlers: Arc::default(),
                        suspend_points: Arc::default(),
                        feedback_sites: Arc::default(),
                        binding_plan: Arc::default(),
                    },
                ),
            ],
            FunctionId::new(0),
        )
        .unwrap()
    }

    /// Builds a callee throw so batch tests cover abrupt exit after an explicit frame switch.
    fn throwing_call_module() -> CompiledModule {
        let span = SourceSpan { start: 0, end: 0 };
        let mut entry = BytecodeBuilder::default();
        entry.emit(Opcode::CreateClosure, &[0, 1], span).unwrap();
        entry.emit(Opcode::Call, &[1, 0, 0], span).unwrap();
        entry.emit(Opcode::Return, &[1], span).unwrap();
        let (entry_bytecode, entry_source_map, entry_registers) = entry.finish().unwrap();

        let mut callee = BytecodeBuilder::default();
        callee.emit(Opcode::LoadImmediate, &[0, 7], span).unwrap();
        callee.emit(Opcode::Throw, &[0], span).unwrap();
        let (callee_bytecode, callee_source_map, callee_registers) = callee.finish().unwrap();

        CompiledModule::new(
            Arc::from("function fail() { throw 7; } fail();"),
            vec![],
            vec![],
            vec![
                CompiledFunctionTemplate::new(
                    FunctionId::new(0),
                    entry_bytecode,
                    FunctionMetadata {
                        kind: FunctionKind::Script,
                        layout: FunctionLayout {
                            register_count: entry_registers,
                            ..FunctionLayout::default()
                        },
                        source_map: entry_source_map,
                        handlers: Arc::default(),
                        suspend_points: Arc::default(),
                        feedback_sites: Arc::default(),
                        binding_plan: Arc::default(),
                    },
                ),
                CompiledFunctionTemplate::new(
                    FunctionId::new(1),
                    callee_bytecode,
                    FunctionMetadata {
                        kind: FunctionKind::Ordinary,
                        layout: FunctionLayout {
                            register_count: callee_registers,
                            ..FunctionLayout::default()
                        },
                        source_map: callee_source_map,
                        handlers: Arc::default(),
                        suspend_points: Arc::default(),
                        feedback_sites: Arc::default(),
                        binding_plan: Arc::default(),
                    },
                ),
            ],
            FunctionId::new(0),
        )
        .unwrap()
    }

    /// Builds separate publisher/caller modules to exercise CodeId changes inside one dispatch batch.
    fn cross_code_modules() -> (CompiledModule, CompiledModule) {
        let span = SourceSpan { start: 0, end: 0 };
        let mut publisher_entry = BytecodeBuilder::default();
        publisher_entry
            .emit(Opcode::CreateClosure, &[0, 1], span)
            .unwrap();
        publisher_entry
            .emit(Opcode::StoreScope, &[0, 0], span)
            .unwrap();
        publisher_entry.emit(Opcode::Return, &[0], span).unwrap();
        let (entry_bytecode, entry_source_map, entry_registers) = publisher_entry.finish().unwrap();
        let mut published_function = BytecodeBuilder::default();
        published_function
            .emit(Opcode::LoadImmediate, &[0, 42], span)
            .unwrap();
        published_function.emit(Opcode::Return, &[0], span).unwrap();
        let (function_bytecode, function_source_map, function_registers) =
            published_function.finish().unwrap();
        let publisher = CompiledModule::new(
            Arc::from("publisher"),
            vec![],
            vec![Arc::from("answer")],
            vec![
                CompiledFunctionTemplate::new(
                    FunctionId::new(0),
                    entry_bytecode,
                    FunctionMetadata {
                        kind: FunctionKind::Script,
                        layout: FunctionLayout {
                            register_count: entry_registers,
                            ..FunctionLayout::default()
                        },
                        source_map: entry_source_map,
                        handlers: Arc::default(),
                        suspend_points: Arc::default(),
                        feedback_sites: Arc::default(),
                        binding_plan: Arc::default(),
                    },
                ),
                CompiledFunctionTemplate::new(
                    FunctionId::new(1),
                    function_bytecode,
                    FunctionMetadata {
                        kind: FunctionKind::Ordinary,
                        layout: FunctionLayout {
                            register_count: function_registers,
                            ..FunctionLayout::default()
                        },
                        source_map: function_source_map,
                        handlers: Arc::default(),
                        suspend_points: Arc::default(),
                        feedback_sites: Arc::default(),
                        binding_plan: Arc::default(),
                    },
                ),
            ],
            FunctionId::new(0),
        )
        .unwrap();

        let mut caller_entry = BytecodeBuilder::default();
        caller_entry.emit(Opcode::LoadScope, &[0, 0], span).unwrap();
        caller_entry.emit(Opcode::Call, &[1, 0, 0], span).unwrap();
        caller_entry.emit(Opcode::Return, &[1], span).unwrap();
        let (caller_bytecode, caller_source_map, caller_registers) = caller_entry.finish().unwrap();
        let caller = CompiledModule::new(
            Arc::from("caller"),
            vec![],
            vec![Arc::from("answer")],
            vec![CompiledFunctionTemplate::new(
                FunctionId::new(0),
                caller_bytecode,
                FunctionMetadata {
                    kind: FunctionKind::Script,
                    layout: FunctionLayout {
                        register_count: caller_registers,
                        ..FunctionLayout::default()
                    },
                    source_map: caller_source_map,
                    handlers: Arc::default(),
                    suspend_points: Arc::default(),
                    feedback_sites: Arc::default(),
                    binding_plan: Arc::default(),
                },
            )],
            FunctionId::new(0),
        )
        .unwrap();
        (publisher, caller)
    }

    fn assert_call_batch<const N: usize>() {
        let outcome = test_isolate()
            .execute_with_batch::<N>(
                &call_module(),
                ExecutionBudget {
                    fuel: 8,
                    quantum: 8,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(42)));
    }

    fn assert_throw_batch<const N: usize>() {
        let outcome = test_isolate()
            .execute_with_batch::<N>(
                &throwing_call_module(),
                ExecutionBudget {
                    fuel: 4,
                    quantum: 4,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Thrown(value) if value.as_i32() == Some(7)));
    }

    fn assert_cross_code_batch<const N: usize>() {
        let (publisher, caller) = cross_code_modules();
        let mut isolate = test_isolate();
        isolate
            .execute_with_batch::<N>(
                &publisher,
                ExecutionBudget {
                    fuel: 8,
                    quantum: 8,
                },
            )
            .unwrap();
        let outcome = isolate
            .execute_with_batch::<N>(
                &caller,
                ExecutionBudget {
                    fuel: 8,
                    quantum: 8,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(42)));
    }

    fn assert_property_batch<const N: usize>() {
        for module in [property_module(), function_property_module()] {
            let outcome = test_isolate()
                .execute_with_batch::<N>(
                    &module,
                    ExecutionBudget {
                        fuel: 32,
                        quantum: 32,
                    },
                )
                .unwrap();
            assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(42)));
        }
    }

    fn assert_dynamic_property_batch<const N: usize>() {
        let outcome = test_isolate()
            .execute_with_batch::<N>(
                &dynamic_property_module(),
                ExecutionBudget {
                    fuel: 8,
                    quantum: 8,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(42)));
    }

    fn assert_method_receiver_batch<const N: usize>() {
        let mut isolate = test_isolate();
        let outcome = isolate
            .execute_with_batch::<N>(
                &method_receiver_module(),
                ExecutionBudget {
                    fuel: 6,
                    quantum: 6,
                },
            )
            .unwrap();
        assert_eq!(outcome, RunOutcome::BudgetExhausted);
        let receiver = isolate.fiber.frames[0].base;
        let receiver = isolate.fiber.registers[receiver as usize];
        assert_eq!(isolate.fiber.frames.last().unwrap().this_value, receiver);
    }

    fn assert_catch_batch<const N: usize>() {
        for module in [direct_catch_module(), cross_frame_catch_module()] {
            let outcome = test_isolate()
                .execute_with_batch::<N>(
                    &module,
                    ExecutionBudget {
                        fuel: 32,
                        quantum: 32,
                    },
                )
                .unwrap();
            assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(42)));
        }
    }

    fn assert_construct_batch<const N: usize>() {
        let outcome = test_isolate()
            .execute_with_batch::<N>(
                &construct_module(),
                ExecutionBudget {
                    fuel: 32,
                    quantum: 32,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(42)));
    }

    fn assert_instanceof_batch<const N: usize>() {
        let outcome = test_isolate()
            .execute_with_batch::<N>(
                &instanceof_module(),
                ExecutionBudget {
                    fuel: 6,
                    quantum: 6,
                },
            )
            .unwrap();
        assert!(matches!(
            outcome,
            RunOutcome::Completed(value)
                if value.as_immediate() == Some(Immediate::True)
        ));
    }

    #[test]
    fn interpreter_executes_int32_arithmetic() {
        assert_batch_result::<1>();
        assert_batch_result::<2>();
        assert_batch_result::<4>();
        assert_batch_result::<8>();
        assert_batch_result::<16>();
    }

    #[test]
    fn numeric_less_than_works_for_every_dispatch_batch() {
        assert_less_than_batch::<1>();
        assert_less_than_batch::<2>();
        assert_less_than_batch::<4>();
        assert_less_than_batch::<8>();
        assert_less_than_batch::<16>();
    }

    #[test]
    fn typeof_and_string_equality_work_for_every_dispatch_batch() {
        assert_typeof_batch::<1>();
        assert_typeof_batch::<2>();
        assert_typeof_batch::<4>();
        assert_typeof_batch::<8>();
        assert_typeof_batch::<16>();
    }

    #[test]
    fn script_var_declaration_works_for_every_dispatch_batch() {
        assert_scope_batch::<1>();
        assert_scope_batch::<2>();
        assert_scope_batch::<4>();
        assert_scope_batch::<8>();
        assert_scope_batch::<16>();
    }

    #[test]
    /// Forces collection between loaded literals so pending and published caches must both trace.
    fn loaded_string_constants_survive_forced_major_during_module_load() {
        let mut isolate = test_isolate();
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
        let outcome = isolate
            .execute(
                &string_constant_root_module(),
                ExecutionBudget {
                    fuel: 4,
                    quantum: 4,
                },
            )
            .unwrap();
        assert!(matches!(
            outcome,
            RunOutcome::Completed(value)
                if value.as_immediate() == Some(Immediate::False)
        ));
    }

    #[test]
    fn isolate_owns_the_atom_table_created_from_mandatory_host_config() {
        let mut isolate = test_isolate();
        let atom = isolate
            .atoms_mut()
            .try_intern(JsString::try_from_str("property").unwrap())
            .unwrap();

        assert_eq!(atom.index(), 0);
        assert_eq!(isolate.atoms().get(atom).unwrap().len(), 8);
        assert_eq!(isolate.atoms().stats().entries, 1);
    }

    #[test]
    fn primitive_realm_intrinsics_precede_unattributed_global_bindings() {
        let mut isolate = test_isolate();
        let infinity = isolate
            .atoms
            .try_intern(JsString::try_from_str("Infinity").unwrap())
            .unwrap();
        isolate.realm.set(infinity, Value::from_i32(1)).unwrap();

        assert_eq!(
            isolate
                .scope_value(ScopeResolution {
                    atom: infinity,
                    global_slot: isolate.realm.resolve(infinity),
                })
                .and_then(Value::as_f64),
            Some(f64::INFINITY)
        );
    }

    #[test]
    fn var_declaration_does_not_publish_over_primitive_realm_intrinsics() {
        let mut isolate = test_isolate();
        let infinity = isolate
            .atoms
            .try_intern(JsString::try_from_str("Infinity").unwrap())
            .unwrap();
        isolate
            .declare_scope_resolution(ScopeResolution {
                atom: infinity,
                global_slot: None,
            })
            .unwrap();
        assert!(isolate.realm.global_bindings.is_empty());
    }

    #[test]
    /// Proves sparse atom publication resolves through stable slots without binding-order scans.
    fn realm_global_slots_are_stable_and_atom_indexed() {
        let mut isolate = test_isolate();
        let unused = isolate
            .atoms
            .try_intern(JsString::try_from_str("unused-property-name").unwrap())
            .unwrap();
        let first = isolate
            .atoms
            .try_intern(JsString::try_from_str("first-global").unwrap())
            .unwrap();
        let second = isolate
            .atoms
            .try_intern(JsString::try_from_str("second-global").unwrap())
            .unwrap();

        isolate.realm.set(first, Value::from_i32(1)).unwrap();
        let first_slot = isolate.realm.resolve(first).unwrap();
        isolate.realm.set(second, Value::from_i32(2)).unwrap();
        isolate.realm.set(first, Value::from_i32(3)).unwrap();

        assert!(isolate.realm.resolve(unused).is_none());
        assert_eq!(isolate.realm.resolve(first), Some(first_slot));
        assert_eq!(
            isolate.realm.get_slot(first_slot).and_then(Value::as_i32),
            Some(3)
        );
        assert_eq!(
            isolate
                .realm
                .resolve(second)
                .and_then(|slot| isolate.realm.get_slot(slot))
                .and_then(Value::as_i32),
            Some(2)
        );
        assert_eq!(
            isolate.realm.global_bindings[first_slot.index()].name,
            first
        );
        assert_eq!(
            isolate.realm.global_slots_by_atom.len(),
            second.index() as usize + 1
        );
    }

    #[test]
    /// Confirms loaded scope operands self-resolve once and retain the stable realm slot.
    fn loaded_scope_resolution_caches_a_published_global_slot() {
        let mut isolate = test_isolate();
        let module = scoped_var_module();
        let code = isolate.load_module(&module).unwrap();
        assert!(
            isolate.loaded_code[code.index()].scope_resolutions[0]
                .global_slot
                .is_none()
        );

        let outcome = isolate
            .execute_loaded(
                code,
                ExecutionBudget {
                    fuel: 16,
                    quantum: 16,
                },
            )
            .unwrap();

        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(7)));
        let resolution = isolate.loaded_code[code.index()].scope_resolutions[0];
        let slot = resolution.global_slot.expect("executed binding is cached");
        assert_eq!(isolate.realm.resolve(resolution.atom), Some(slot));
        assert_eq!(
            isolate.realm.get_slot(slot).and_then(Value::as_i32),
            Some(7)
        );
    }

    #[test]
    fn interpreter_stops_at_exact_budget_boundary() {
        assert_batch_budget::<1>();
        assert_batch_budget::<2>();
        assert_batch_budget::<4>();
        assert_batch_budget::<8>();
        assert_batch_budget::<16>();
    }

    #[test]
    fn interpreter_rejects_zero_batch_without_panicking() {
        let result = test_isolate().execute_with_batch::<0>(
            &arithmetic_module(),
            ExecutionBudget {
                fuel: 1,
                quantum: 1,
            },
        );
        assert_eq!(
            result,
            Err(ExecutionError::InvalidDispatchBatch { batch: 0 })
        );
    }

    #[test]
    fn public_execute_uses_the_tuned_non_scalar_dispatch_batch() {
        assert_eq!(tuning::dispatch::DEFAULT_DISPATCH_BATCH, 8);
        let outcome = test_isolate()
            .execute(
                &arithmetic_module(),
                ExecutionBudget {
                    fuel: 4,
                    quantum: 4,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(3)));
    }

    #[test]
    fn interpreter_restarts_dispatch_after_conditional_jumps() {
        assert_conditional_batch::<1>();
        assert_conditional_batch::<2>();
        assert_conditional_batch::<4>();
        assert_conditional_batch::<8>();
        assert_conditional_batch::<16>();
    }

    #[test]
    fn logical_short_circuit_preserves_operands_for_every_dispatch_batch() {
        assert_logical_batch::<1>();
        assert_logical_batch::<2>();
        assert_logical_batch::<4>();
        assert_logical_batch::<8>();
        assert_logical_batch::<16>();
    }

    #[test]
    fn switch_dispatch_chain_is_stable_for_every_dispatch_batch() {
        assert_switch_batch::<1>();
        assert_switch_batch::<2>();
        assert_switch_batch::<4>();
        assert_switch_batch::<8>();
        assert_switch_batch::<16>();
    }

    #[test]
    fn explicit_function_frames_work_for_every_dispatch_batch() {
        assert_call_batch::<1>();
        assert_call_batch::<2>();
        assert_call_batch::<4>();
        assert_call_batch::<8>();
        assert_call_batch::<16>();
    }

    #[test]
    fn callee_throw_exits_every_dispatch_batch_without_native_unwind() {
        assert_throw_batch::<1>();
        assert_throw_batch::<2>();
        assert_throw_batch::<4>();
        assert_throw_batch::<8>();
        assert_throw_batch::<16>();
    }

    #[test]
    fn cross_module_call_switches_code_for_every_dispatch_batch() {
        assert_cross_code_batch::<1>();
        assert_cross_code_batch::<2>();
        assert_cross_code_batch::<4>();
        assert_cross_code_batch::<8>();
        assert_cross_code_batch::<16>();
    }

    #[test]
    fn ordinary_property_replacement_and_update_work_for_every_dispatch_batch() {
        assert_property_batch::<1>();
        assert_property_batch::<2>();
        assert_property_batch::<4>();
        assert_property_batch::<8>();
        assert_property_batch::<16>();
    }

    #[test]
    fn computed_property_access_works_for_every_dispatch_batch() {
        assert_dynamic_property_batch::<1>();
        assert_dynamic_property_batch::<2>();
        assert_dynamic_property_batch::<4>();
        assert_dynamic_property_batch::<8>();
        assert_dynamic_property_batch::<16>();
    }

    #[test]
    /// Forces collection during first numeric-key publication to exercise the shared rooting path.
    fn computed_property_publication_roots_receiver_across_forced_major() {
        let mut isolate = test_isolate();
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
        let outcome = isolate
            .execute(
                &dynamic_property_module(),
                ExecutionBudget {
                    fuel: 8,
                    quantum: 8,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(42)));
    }

    #[test]
    fn integer_property_keys_preserve_ecmascript_decimal_spelling() {
        for (value, expected) in [
            (i32::MIN, "-2147483648"),
            (-1, "-1"),
            (0, "0"),
            (i32::MAX, "2147483647"),
        ] {
            let key = Int32PropertyKey::new(value);
            assert_eq!(key.as_bytes(), expected.as_bytes());
        }
    }

    #[test]
    fn method_calls_preserve_receiver_for_every_dispatch_batch() {
        assert_method_receiver_batch::<1>();
        assert_method_receiver_batch::<2>();
        assert_method_receiver_batch::<4>();
        assert_method_receiver_batch::<8>();
        assert_method_receiver_batch::<16>();
    }

    #[test]
    fn catch_dispatch_and_cross_frame_throw_work_for_every_dispatch_batch() {
        assert_catch_batch::<1>();
        assert_catch_batch::<2>();
        assert_catch_batch::<4>();
        assert_catch_batch::<8>();
        assert_catch_batch::<16>();
    }

    #[test]
    fn construct_receiver_and_primitive_return_work_for_every_dispatch_batch() {
        assert_construct_batch::<1>();
        assert_construct_batch::<2>();
        assert_construct_batch::<4>();
        assert_construct_batch::<8>();
        assert_construct_batch::<16>();
    }

    #[test]
    fn instanceof_walks_prototypes_for_every_dispatch_batch() {
        assert_instanceof_batch::<1>();
        assert_instanceof_batch::<2>();
        assert_instanceof_batch::<4>();
        assert_instanceof_batch::<8>();
        assert_instanceof_batch::<16>();
    }

    #[test]
    /// Forced major collections cover closure prototype creation and receiver chain publication.
    fn instanceof_prototype_chain_survives_forced_major() {
        let mut isolate = test_isolate();
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
        let outcome = isolate
            .execute(
                &instanceof_module(),
                ExecutionBudget {
                    fuel: 6,
                    quantum: 6,
                },
            )
            .unwrap();
        assert!(matches!(
            outcome,
            RunOutcome::Completed(value)
                if value.as_immediate() == Some(Immediate::True)
        ));
    }

    #[test]
    /// Plain closure creation stays allocation-light until prototype observation materializes it.
    fn function_prototype_is_lazily_materialized_with_constructor_back_reference() {
        let mut isolate = test_isolate();
        let outcome = isolate
            .execute_with_batch::<8>(
                &call_module(),
                ExecutionBudget {
                    fuel: 1,
                    quantum: 1,
                },
            )
            .unwrap();
        assert_eq!(outcome, RunOutcome::BudgetExhausted);
        let function = isolate.fiber.registers[0];
        let reference = isolate
            .heap
            .checked_reference(function.as_heap_ref().unwrap(), isolate.types.function)
            .unwrap();
        let before = isolate.heap.with_running_scope(|scope| {
            let function = scope.root(reference).unwrap();
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(function, isolate.types.function)
                    .unwrap()
                    .function_prototype
            })
        });
        assert!(before.is_none());

        let prototype_atom = isolate.prototype_atom().unwrap();
        let prototype = isolate
            .get_data_property(function, prototype_atom)
            .unwrap()
            .unwrap();
        let constructor_atom = isolate.constructor_atom().unwrap();
        assert_eq!(
            isolate
                .get_data_property(prototype, constructor_atom)
                .unwrap(),
            Some(function)
        );
    }

    #[test]
    fn construct_receiver_stays_rooted_across_forced_major_property_allocation() {
        let mut isolate = test_isolate();
        isolate
            .heap
            .set_forced_collection_mode(ForcedCollectionMode::Major);
        let outcome = isolate
            .execute(
                &construct_module(),
                ExecutionBudget {
                    fuel: 32,
                    quantum: 32,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(42)));
    }

    #[test]
    fn property_publication_roots_receiver_and_heap_value_across_forced_major() {
        for module in [
            heap_value_property_module(),
            function_heap_value_property_module(),
        ] {
            let mut isolate = test_isolate();
            isolate
                .heap
                .set_forced_collection_mode(ForcedCollectionMode::Major);
            let outcome = isolate
                .execute(
                    &module,
                    ExecutionBudget {
                        fuel: 16,
                        quantum: 16,
                    },
                )
                .unwrap();
            let RunOutcome::Completed(child) = outcome else {
                panic!("property fixture must complete");
            };
            assert!(isolate.object_snapshot(child).is_ok());
        }
    }

    #[test]
    /// Confirms verifier-produced stack depths become allocation reservations before dispatch.
    fn entry_reserves_verified_execution_windows() {
        let layout = FunctionLayout {
            register_count: 2,
            max_handler_depth: 3,
            max_completion_depth: 4,
            ..FunctionLayout::default()
        };
        let mut isolate = test_isolate();
        let module = state_module(FunctionKind::Module, layout);
        let code = isolate.load_module(&module).unwrap();
        isolate.enter(code, FunctionId::new(0)).unwrap();

        assert!(isolate.fiber.frames.capacity() >= 1);
        assert!(isolate.fiber.registers.capacity() >= 2);
        assert!(isolate.fiber.handlers.capacity() >= 3);
        assert!(isolate.fiber.completions.capacity() >= 4);
        assert_eq!(isolate.fiber.frames[0].strictness, Strictness::Strict);
    }

    #[test]
    /// Exercises every fiber-owned GC edge with a tracer that simulates object relocation.
    fn fiber_trace_roots_visits_execution_state_without_native_stack_scanning() {
        let layout = FunctionLayout {
            register_count: 1,
            max_handler_depth: 1,
            max_completion_depth: 2,
            ..FunctionLayout::default()
        };
        let mut isolate = test_isolate();
        let module = state_module(FunctionKind::Script, layout);
        let code = isolate.load_module(&module).unwrap();
        isolate.enter(code, FunctionId::new(0)).unwrap();
        let raw = RawHeapRef::new(16).expect("valid logical address");
        isolate.fiber.registers[0] = Value::from_heap_ref(raw);
        let global = isolate
            .atoms
            .try_intern(JsString::try_from_str("global").unwrap())
            .unwrap();
        isolate
            .realm
            .set(global, Value::from_heap_ref(raw))
            .unwrap();
        let frame = isolate.fiber.frames.last_mut().expect("entry frame exists");
        // SAFETY: this test never dereferences the synthetic environment reference; it only checks
        // that the exact tracing contract rewrites every encoded fiber edge.
        frame.environment = Some(unsafe { GcRef::from_raw_unchecked(raw) });
        frame.return_register = Some(RegisterId::new(0));
        frame.this_value = Value::from_heap_ref(raw);
        frame.new_target = Value::from_heap_ref(raw);
        isolate.fiber.handlers.push(ActiveHandler {
            handler_index: 0,
            frame_depth: 1,
            environment_depth: 1,
        });
        isolate.fiber.completions.extend([
            Completion::Return(Value::from_heap_ref(raw)),
            Completion::Throw(Value::from_heap_ref(raw)),
        ]);
        isolate.fiber.pending_exception = Some(Value::from_heap_ref(raw));
        let mut tracer = RewritingTracer;

        isolate.trace_roots(&mut tracer);

        let rewritten = RawHeapRef::new(32).expect("valid logical address");
        assert_eq!(isolate.fiber.registers[0].as_heap_ref(), Some(rewritten));
        let frame = isolate.fiber.frames.last().expect("entry frame exists");
        assert_eq!(frame.environment.map(GcRef::raw), Some(rewritten));
        assert_eq!(frame.this_value.as_heap_ref(), Some(rewritten));
        assert_eq!(frame.new_target.as_heap_ref(), Some(rewritten));
        assert!(matches!(
            isolate.fiber.completions[0],
            Completion::Return(value) if value.as_heap_ref() == Some(rewritten)
        ));
        assert_eq!(
            isolate.fiber.pending_exception.and_then(Value::as_heap_ref),
            Some(rewritten)
        );
        assert!(matches!(
            isolate.fiber.completions[1],
            Completion::Throw(value) if value.as_heap_ref() == Some(rewritten)
        ));
        assert_eq!(
            isolate
                .realm
                .resolve(global)
                .and_then(|slot| isolate.realm.get_slot(slot))
                .and_then(Value::as_heap_ref),
            Some(rewritten)
        );
    }

    struct RewritingTracer;

    impl Tracer for RewritingTracer {
        fn trace_value(&mut self, value: &mut Value) {
            if let Some(reference) = value.as_heap_ref() {
                *value = Value::from_heap_ref(rewrite(reference));
            }
        }

        fn trace_raw_heap_ref(&mut self, reference: &mut RawHeapRef) {
            *reference = rewrite(*reference);
        }

        fn trace_weak_raw_heap_ref(&mut self, reference: &mut Option<RawHeapRef>) {
            *reference = reference.map(rewrite);
        }

        fn trace_ephemeron(&mut self, key: &mut Option<RawHeapRef>, value: &mut Value) {
            *key = key.map(rewrite);
            self.trace_value(value);
        }

        fn trace_finalization(&mut self, target: &mut Option<RawHeapRef>, held_value: &mut Value) {
            *target = target.map(rewrite);
            self.trace_value(held_value);
        }
    }

    fn rewrite(reference: RawHeapRef) -> RawHeapRef {
        RawHeapRef::new(reference.offset() + 16).expect("test offset stays non-zero")
    }

    fn assert_batch_result<const N: usize>() {
        let outcome = test_isolate()
            .execute_with_batch::<N>(
                &arithmetic_module(),
                ExecutionBudget {
                    fuel: 4,
                    quantum: 4,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(3)));
    }

    fn assert_less_than_batch<const N: usize>() {
        let outcome = test_isolate()
            .execute_with_batch::<N>(
                &less_than_module(),
                ExecutionBudget {
                    fuel: 4,
                    quantum: 4,
                },
            )
            .unwrap();
        assert!(matches!(
            outcome,
            RunOutcome::Completed(value)
                if value.as_immediate() == Some(Immediate::True)
        ));
    }

    fn assert_typeof_batch<const N: usize>() {
        let outcome = test_isolate()
            .execute_with_batch::<N>(
                &typeof_module(),
                ExecutionBudget {
                    fuel: 6,
                    quantum: 6,
                },
            )
            .unwrap();
        assert!(matches!(
            outcome,
            RunOutcome::Completed(value)
                if value.as_immediate() == Some(Immediate::True)
        ));
    }

    fn assert_scope_batch<const N: usize>() {
        let outcome = test_isolate()
            .execute_with_batch::<N>(
                &scoped_var_module(),
                ExecutionBudget {
                    fuel: 6,
                    quantum: 6,
                },
            )
            .unwrap();
        assert!(matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(7)));
    }

    fn assert_batch_budget<const N: usize>() {
        let outcome = test_isolate()
            .execute_with_batch::<N>(
                &arithmetic_module(),
                ExecutionBudget {
                    fuel: 3,
                    quantum: 3,
                },
            )
            .unwrap();
        assert_eq!(outcome, RunOutcome::BudgetExhausted);
    }

    fn assert_conditional_batch<const N: usize>() {
        for (test, expected) in [(Opcode::LoadTrue, 1), (Opcode::LoadFalse, 2)] {
            let outcome = test_isolate()
                .execute_with_batch::<N>(
                    &conditional_module(test),
                    ExecutionBudget {
                        fuel: 6,
                        quantum: 6,
                    },
                )
                .unwrap();
            assert!(
                matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(expected))
            );
        }
    }

    /// Checks both paths of all logical branches under one dispatch batch monomorphization.
    fn assert_logical_batch<const N: usize>() {
        let cases = [
            (Opcode::JumpIfFalse, Opcode::LoadFalse, None, None),
            (Opcode::JumpIfFalse, Opcode::LoadTrue, None, Some(42)),
            (Opcode::JumpIfTrue, Opcode::LoadFalse, None, Some(42)),
            (Opcode::JumpIfTrue, Opcode::LoadTrue, None, None),
            (Opcode::JumpIfNotNullish, Opcode::LoadNull, None, Some(42)),
            (
                Opcode::JumpIfNotNullish,
                Opcode::LoadImmediate,
                Some(7),
                Some(7),
            ),
        ];
        for (branch, left, immediate, expected_integer) in cases {
            let outcome = test_isolate()
                .execute_with_batch::<N>(
                    &logical_module(branch, left, immediate),
                    ExecutionBudget {
                        fuel: 8,
                        quantum: 8,
                    },
                )
                .unwrap();
            let RunOutcome::Completed(value) = outcome else {
                panic!("logical module must complete");
            };
            if let Some(expected_integer) = expected_integer {
                assert_eq!(value.as_i32(), Some(expected_integer));
            } else {
                let expected = if left == Opcode::LoadFalse {
                    Immediate::False
                } else {
                    Immediate::True
                };
                assert_eq!(value.as_immediate(), Some(expected));
            }
        }
    }

    fn assert_switch_batch<const N: usize>() {
        for (discriminant, expected) in [(1, 10), (2, 20), (9, 20)] {
            let outcome = test_isolate()
                .execute_with_batch::<N>(
                    &switch_module(discriminant),
                    ExecutionBudget {
                        fuel: 16,
                        quantum: 16,
                    },
                )
                .unwrap();
            assert!(
                matches!(outcome, RunOutcome::Completed(value) if value.as_i32() == Some(expected))
            );
        }
    }

    /// Builds a verified branch program with explicit labels to exercise PC changes inside one dispatch batch.
    fn conditional_module(test: Opcode) -> CompiledModule {
        let span = SourceSpan { start: 0, end: 1 };
        let mut builder = BytecodeBuilder::with_capacity(8, 2);
        let alternate = builder.new_label().unwrap();
        let end = builder.new_label().unwrap();
        builder.emit(test, &[0], span).unwrap();
        builder
            .emit_jump_if_false(RegisterId::new(0), alternate, span)
            .unwrap();
        builder.emit(Opcode::LoadImmediate, &[1, 1], span).unwrap();
        builder.emit_jump(end, span).unwrap();
        builder.bind_label(alternate).unwrap();
        builder.emit(Opcode::LoadImmediate, &[1, 2], span).unwrap();
        builder.bind_label(end).unwrap();
        builder.emit(Opcode::Return, &[1], span).unwrap();
        let (bytecode, source_map, register_count) = builder.finish().unwrap();
        let metadata = FunctionMetadata {
            layout: FunctionLayout {
                register_count,
                ..FunctionLayout::default()
            },
            source_map,
            ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
        };
        CompiledModule::new(
            Arc::from("conditional"),
            Vec::new(),
            Vec::new(),
            vec![CompiledFunctionTemplate::new(
                FunctionId::new(0),
                bytecode,
                metadata,
            )],
            FunctionId::new(0),
        )
        .unwrap()
    }

    /// Builds one operand-preserving short-circuit branch around a right-hand integer load.
    fn logical_module(branch: Opcode, left: Opcode, immediate: Option<u32>) -> CompiledModule {
        let span = SourceSpan { start: 0, end: 1 };
        let mut builder = BytecodeBuilder::with_capacity(6, 1);
        let end = builder.new_label().unwrap();
        if let Some(value) = immediate {
            builder.emit(left, &[0, value], span).unwrap();
        } else {
            builder.emit(left, &[0], span).unwrap();
        }
        builder.emit(Opcode::Move, &[1, 0], span).unwrap();
        match branch {
            Opcode::JumpIfFalse => builder.emit_jump_if_false(RegisterId::new(0), end, span),
            Opcode::JumpIfTrue => builder.emit_jump_if_true(RegisterId::new(0), end, span),
            Opcode::JumpIfNotNullish => {
                builder.emit_jump_if_not_nullish(RegisterId::new(0), end, span)
            }
            _ => panic!("test supplies a logical branch opcode"),
        }
        .unwrap();
        builder.emit(Opcode::LoadImmediate, &[2, 42], span).unwrap();
        builder.emit(Opcode::Move, &[1, 2], span).unwrap();
        builder.bind_label(end).unwrap();
        builder.emit(Opcode::Return, &[1], span).unwrap();
        let (bytecode, source_map, register_count) = builder.finish().unwrap();
        let metadata = FunctionMetadata {
            layout: FunctionLayout {
                register_count,
                ..FunctionLayout::default()
            },
            source_map,
            ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
        };
        CompiledModule::new(
            Arc::from("logical"),
            Vec::new(),
            Vec::new(),
            vec![CompiledFunctionTemplate::new(
                FunctionId::new(0),
                bytecode,
                metadata,
            )],
            FunctionId::new(0),
        )
        .unwrap()
    }

    /// Builds a two-case dispatch whose middle default deliberately falls through into case two.
    fn switch_module(discriminant: u32) -> CompiledModule {
        let span = SourceSpan { start: 0, end: 1 };
        let mut builder = BytecodeBuilder::with_capacity(16, 4);
        let case_one = builder.new_label().unwrap();
        let default = builder.new_label().unwrap();
        let case_two = builder.new_label().unwrap();
        let end = builder.new_label().unwrap();
        builder
            .emit(Opcode::LoadImmediate, &[0, discriminant], span)
            .unwrap();
        builder.emit(Opcode::LoadImmediate, &[1, 1], span).unwrap();
        builder.emit(Opcode::StrictEqual, &[2, 0, 1], span).unwrap();
        builder
            .emit_jump_if_true(RegisterId::new(2), case_one, span)
            .unwrap();
        builder.emit(Opcode::LoadImmediate, &[3, 2], span).unwrap();
        builder.emit(Opcode::StrictEqual, &[4, 0, 3], span).unwrap();
        builder
            .emit_jump_if_true(RegisterId::new(4), case_two, span)
            .unwrap();
        builder.emit_jump(default, span).unwrap();
        builder.bind_label(case_one).unwrap();
        builder.emit(Opcode::LoadImmediate, &[5, 10], span).unwrap();
        builder.emit_jump(end, span).unwrap();
        builder.bind_label(default).unwrap();
        builder.emit(Opcode::LoadImmediate, &[5, 30], span).unwrap();
        builder.bind_label(case_two).unwrap();
        builder.emit(Opcode::LoadImmediate, &[5, 20], span).unwrap();
        builder.emit_jump(end, span).unwrap();
        builder.bind_label(end).unwrap();
        builder.emit(Opcode::Return, &[5], span).unwrap();
        let (bytecode, source_map, register_count) = builder.finish().unwrap();
        let metadata = FunctionMetadata {
            layout: FunctionLayout {
                register_count,
                ..FunctionLayout::default()
            },
            source_map,
            ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
        };
        CompiledModule::new(
            Arc::from("switch"),
            Vec::new(),
            Vec::new(),
            vec![CompiledFunctionTemplate::new(
                FunctionId::new(0),
                bytecode,
                metadata,
            )],
            FunctionId::new(0),
        )
        .unwrap()
    }

    /// Builds two property additions followed by an allocation-free update and own read.
    fn property_module() -> CompiledModule {
        let span = SourceSpan { start: 0, end: 1 };
        let mut builder = BytecodeBuilder::with_capacity(10, 0);
        builder.emit(Opcode::CreateObject, &[0], span).unwrap();
        builder.emit(Opcode::LoadImmediate, &[1, 41], span).unwrap();
        builder.emit(Opcode::SetById, &[0, 1, 0], span).unwrap();
        builder.emit(Opcode::LoadImmediate, &[2, 7], span).unwrap();
        builder.emit(Opcode::SetById, &[0, 2, 1], span).unwrap();
        builder.emit(Opcode::LoadImmediate, &[3, 42], span).unwrap();
        builder.emit(Opcode::SetById, &[0, 3, 0], span).unwrap();
        builder.emit(Opcode::GetById, &[4, 0, 0], span).unwrap();
        builder.emit(Opcode::Return, &[4], span).unwrap();
        let (bytecode, source_map, register_count) = builder.finish().unwrap();
        let metadata = FunctionMetadata {
            layout: FunctionLayout {
                register_count,
                ..FunctionLayout::default()
            },
            source_map,
            ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
        };
        CompiledModule::new(
            Arc::from("properties"),
            Vec::new(),
            vec![Arc::from("answer"), Arc::from("other")],
            vec![CompiledFunctionTemplate::new(
                FunctionId::new(0),
                bytecode,
                metadata,
            )],
            FunctionId::new(0),
        )
        .unwrap()
    }

    /// Builds a numeric-key write/read pair over one ordinary object.
    fn dynamic_property_module() -> CompiledModule {
        let span = SourceSpan { start: 0, end: 1 };
        let mut builder = BytecodeBuilder::with_capacity(6, 0);
        builder.emit(Opcode::CreateObject, &[0], span).unwrap();
        builder.emit(Opcode::LoadImmediate, &[1, 0], span).unwrap();
        builder.emit(Opcode::LoadImmediate, &[2, 42], span).unwrap();
        builder.emit(Opcode::SetByValue, &[0, 2, 1], span).unwrap();
        builder.emit(Opcode::GetByValue, &[3, 0, 1], span).unwrap();
        builder.emit(Opcode::Return, &[3], span).unwrap();
        let (bytecode, source_map, register_count) = builder.finish().unwrap();
        let metadata = FunctionMetadata {
            layout: FunctionLayout {
                register_count,
                ..FunctionLayout::default()
            },
            source_map,
            ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
        };
        CompiledModule::new(
            Arc::from("dynamic properties"),
            Vec::new(),
            Vec::new(),
            vec![CompiledFunctionTemplate::new(
                FunctionId::new(0),
                bytecode,
                metadata,
            )],
            FunctionId::new(0),
        )
        .unwrap()
    }

    /// Builds a callable carrying the same shape/storage path as an ordinary object.
    fn function_property_module() -> CompiledModule {
        let span = SourceSpan { start: 0, end: 1 };
        let mut entry = BytecodeBuilder::with_capacity(6, 0);
        entry.emit(Opcode::CreateClosure, &[0, 1], span).unwrap();
        entry.emit(Opcode::LoadImmediate, &[1, 42], span).unwrap();
        entry.emit(Opcode::SetById, &[0, 1, 0], span).unwrap();
        entry.emit(Opcode::GetById, &[2, 0, 0], span).unwrap();
        entry.emit(Opcode::Return, &[2], span).unwrap();
        let (entry_bytecode, entry_source_map, entry_registers) = entry.finish().unwrap();
        let mut callee = BytecodeBuilder::with_capacity(2, 0);
        callee.emit(Opcode::LoadUndefined, &[0], span).unwrap();
        callee.emit(Opcode::Return, &[0], span).unwrap();
        let (callee_bytecode, callee_source_map, callee_registers) = callee.finish().unwrap();
        let entry_metadata = FunctionMetadata {
            layout: FunctionLayout {
                register_count: entry_registers,
                ..FunctionLayout::default()
            },
            source_map: entry_source_map,
            ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
        };
        let callee_metadata = FunctionMetadata {
            layout: FunctionLayout {
                register_count: callee_registers,
                ..FunctionLayout::default()
            },
            source_map: callee_source_map,
            ..FunctionMetadata::new(FunctionKind::Ordinary, FunctionLayout::default())
        };
        CompiledModule::new(
            Arc::from("function property"),
            Vec::new(),
            vec![Arc::from("answer")],
            vec![
                CompiledFunctionTemplate::new(FunctionId::new(0), entry_bytecode, entry_metadata),
                CompiledFunctionTemplate::new(FunctionId::new(1), callee_bytecode, callee_metadata),
            ],
            FunctionId::new(0),
        )
        .unwrap()
    }

    /// Builds a method call that stops immediately after pushing its receiver-bearing frame.
    fn method_receiver_module() -> CompiledModule {
        let span = SourceSpan { start: 0, end: 1 };
        let mut entry = BytecodeBuilder::with_capacity(8, 0);
        entry.emit(Opcode::CreateObject, &[0], span).unwrap();
        entry.emit(Opcode::CreateClosure, &[1, 1], span).unwrap();
        entry.emit(Opcode::SetById, &[0, 1, 0], span).unwrap();
        entry.emit(Opcode::Move, &[2, 0], span).unwrap();
        entry.emit(Opcode::GetById, &[3, 2, 0], span).unwrap();
        entry
            .emit(Opcode::CallWithReceiver, &[4, 2, 0], span)
            .unwrap();
        entry.emit(Opcode::Return, &[4], span).unwrap();
        let (entry_bytecode, entry_source_map, entry_registers) = entry.finish().unwrap();
        let mut callee = BytecodeBuilder::with_capacity(2, 0);
        callee.emit(Opcode::LoadUndefined, &[0], span).unwrap();
        callee.emit(Opcode::Return, &[0], span).unwrap();
        let (callee_bytecode, callee_source_map, callee_registers) = callee.finish().unwrap();
        let entry_metadata = FunctionMetadata {
            layout: FunctionLayout {
                register_count: entry_registers,
                ..FunctionLayout::default()
            },
            source_map: entry_source_map,
            ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
        };
        let callee_metadata = FunctionMetadata {
            layout: FunctionLayout {
                register_count: callee_registers,
                ..FunctionLayout::default()
            },
            source_map: callee_source_map,
            ..FunctionMetadata::new(FunctionKind::Ordinary, FunctionLayout::default())
        };
        CompiledModule::new(
            Arc::from("method receiver"),
            Vec::new(),
            vec![Arc::from("method")],
            vec![
                CompiledFunctionTemplate::new(FunctionId::new(0), entry_bytecode, entry_metadata),
                CompiledFunctionTemplate::new(FunctionId::new(1), callee_bytecode, callee_metadata),
            ],
            FunctionId::new(0),
        )
        .unwrap()
    }

    /// Stores a young object through an allocation that forces root tracing before publication.
    fn heap_value_property_module() -> CompiledModule {
        let span = SourceSpan { start: 0, end: 1 };
        let mut builder = BytecodeBuilder::with_capacity(6, 0);
        builder.emit(Opcode::CreateObject, &[0], span).unwrap();
        builder.emit(Opcode::CreateObject, &[1], span).unwrap();
        builder.emit(Opcode::SetById, &[0, 1, 0], span).unwrap();
        builder.emit(Opcode::GetById, &[2, 0, 0], span).unwrap();
        builder.emit(Opcode::Return, &[2], span).unwrap();
        let (bytecode, source_map, register_count) = builder.finish().unwrap();
        let metadata = FunctionMetadata {
            layout: FunctionLayout {
                register_count,
                ..FunctionLayout::default()
            },
            source_map,
            ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
        };
        CompiledModule::new(
            Arc::from("heap value property"),
            Vec::new(),
            vec![Arc::from("child")],
            vec![CompiledFunctionTemplate::new(
                FunctionId::new(0),
                bytecode,
                metadata,
            )],
            FunctionId::new(0),
        )
        .unwrap()
    }

    /// Stores a young object through a callable's embedded ordinary-property edge.
    fn function_heap_value_property_module() -> CompiledModule {
        let span = SourceSpan { start: 0, end: 1 };
        let mut entry = BytecodeBuilder::with_capacity(6, 0);
        entry.emit(Opcode::CreateClosure, &[0, 1], span).unwrap();
        entry.emit(Opcode::CreateObject, &[1], span).unwrap();
        entry.emit(Opcode::SetById, &[0, 1, 0], span).unwrap();
        entry.emit(Opcode::GetById, &[2, 0, 0], span).unwrap();
        entry.emit(Opcode::Return, &[2], span).unwrap();
        let (entry_bytecode, entry_source_map, entry_registers) = entry.finish().unwrap();
        let mut callee = BytecodeBuilder::with_capacity(2, 0);
        callee.emit(Opcode::LoadUndefined, &[0], span).unwrap();
        callee.emit(Opcode::Return, &[0], span).unwrap();
        let (callee_bytecode, callee_source_map, callee_registers) = callee.finish().unwrap();
        let entry_metadata = FunctionMetadata {
            layout: FunctionLayout {
                register_count: entry_registers,
                ..FunctionLayout::default()
            },
            source_map: entry_source_map,
            ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
        };
        let callee_metadata = FunctionMetadata {
            layout: FunctionLayout {
                register_count: callee_registers,
                ..FunctionLayout::default()
            },
            source_map: callee_source_map,
            ..FunctionMetadata::new(FunctionKind::Ordinary, FunctionLayout::default())
        };
        CompiledModule::new(
            Arc::from("function heap property"),
            Vec::new(),
            vec![Arc::from("child")],
            vec![
                CompiledFunctionTemplate::new(FunctionId::new(0), entry_bytecode, entry_metadata),
                CompiledFunctionTemplate::new(FunctionId::new(1), callee_bytecode, callee_metadata),
            ],
            FunctionId::new(0),
        )
        .unwrap()
    }

    /// Builds a same-frame catch range with a pending-exception load at its handler target.
    fn direct_catch_module() -> CompiledModule {
        let span = SourceSpan { start: 0, end: 1 };
        let mut builder = BytecodeBuilder::with_capacity(10, 1);
        let end = builder.new_label().unwrap();
        let protected_start = builder.emit(Opcode::Nop, &[], span).unwrap();
        builder.emit(Opcode::LoadImmediate, &[0, 42], span).unwrap();
        builder.emit(Opcode::Throw, &[0], span).unwrap();
        builder.emit_jump(end, span).unwrap();
        let handler = builder.emit(Opcode::LoadException, &[1], span).unwrap();
        builder.emit(Opcode::Return, &[1], span).unwrap();
        builder.bind_label(end).unwrap();
        builder.emit(Opcode::LoadUndefined, &[2], span).unwrap();
        builder.emit(Opcode::Return, &[2], span).unwrap();
        let (bytecode, source_map, register_count) = builder.finish().unwrap();
        let mut metadata = FunctionMetadata {
            layout: FunctionLayout {
                register_count,
                max_handler_depth: 1,
                ..FunctionLayout::default()
            },
            source_map,
            ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
        };
        metadata.handlers = vec![HandlerEntry {
            protected_start,
            protected_end: handler,
            handler,
            kind: HandlerKind::Catch,
            environment_depth: 0,
        }]
        .into();
        CompiledModule::new(
            Arc::from("direct catch"),
            Vec::new(),
            Vec::new(),
            vec![CompiledFunctionTemplate::new(
                FunctionId::new(0),
                bytecode,
                metadata,
            )],
            FunctionId::new(0),
        )
        .unwrap()
    }

    /// Builds a caller handler around a call whose callee throws through its explicit frame.
    fn cross_frame_catch_module() -> CompiledModule {
        let span = SourceSpan { start: 0, end: 1 };
        let mut entry = BytecodeBuilder::with_capacity(12, 1);
        let end = entry.new_label().unwrap();
        entry.emit(Opcode::CreateClosure, &[0, 1], span).unwrap();
        let protected_start = entry.emit(Opcode::Nop, &[], span).unwrap();
        entry.emit(Opcode::Call, &[1, 0, 0], span).unwrap();
        entry.emit_jump(end, span).unwrap();
        let handler = entry.emit(Opcode::LoadException, &[2], span).unwrap();
        entry.emit(Opcode::Return, &[2], span).unwrap();
        entry.bind_label(end).unwrap();
        entry.emit(Opcode::LoadUndefined, &[3], span).unwrap();
        entry.emit(Opcode::Return, &[3], span).unwrap();
        let (entry_bytecode, entry_source_map, entry_registers) = entry.finish().unwrap();
        let mut callee = BytecodeBuilder::with_capacity(2, 0);
        callee.emit(Opcode::LoadImmediate, &[0, 42], span).unwrap();
        callee.emit(Opcode::Throw, &[0], span).unwrap();
        let (callee_bytecode, callee_source_map, callee_registers) = callee.finish().unwrap();
        let mut entry_metadata = FunctionMetadata {
            layout: FunctionLayout {
                register_count: entry_registers,
                max_handler_depth: 1,
                ..FunctionLayout::default()
            },
            source_map: entry_source_map,
            ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
        };
        entry_metadata.handlers = vec![HandlerEntry {
            protected_start,
            protected_end: handler,
            handler,
            kind: HandlerKind::Catch,
            environment_depth: 0,
        }]
        .into();
        let callee_metadata = FunctionMetadata {
            layout: FunctionLayout {
                register_count: callee_registers,
                ..FunctionLayout::default()
            },
            source_map: callee_source_map,
            ..FunctionMetadata::new(FunctionKind::Ordinary, FunctionLayout::default())
        };
        CompiledModule::new(
            Arc::from("cross-frame catch"),
            Vec::new(),
            Vec::new(),
            vec![
                CompiledFunctionTemplate::new(FunctionId::new(0), entry_bytecode, entry_metadata),
                CompiledFunctionTemplate::new(FunctionId::new(1), callee_bytecode, callee_metadata),
            ],
            FunctionId::new(0),
        )
        .unwrap()
    }

    /// Builds a constructor that stores one argument on `this` and returns a primitive fallback.
    fn construct_module() -> CompiledModule {
        let span = SourceSpan { start: 0, end: 1 };
        let mut entry = BytecodeBuilder::with_capacity(6, 0);
        entry.emit(Opcode::CreateClosure, &[0, 1], span).unwrap();
        entry.emit(Opcode::LoadImmediate, &[1, 42], span).unwrap();
        entry.emit(Opcode::Construct, &[2, 0, 1], span).unwrap();
        entry.emit(Opcode::GetById, &[3, 2, 0], span).unwrap();
        entry.emit(Opcode::Return, &[3], span).unwrap();
        let (entry_bytecode, entry_source_map, entry_registers) = entry.finish().unwrap();
        let mut constructor = BytecodeBuilder::with_capacity(6, 0);
        constructor.emit(Opcode::LoadThis, &[1], span).unwrap();
        constructor.emit(Opcode::SetById, &[1, 0, 0], span).unwrap();
        constructor
            .emit(Opcode::LoadImmediate, &[2, 7], span)
            .unwrap();
        constructor.emit(Opcode::Return, &[2], span).unwrap();
        let (constructor_bytecode, constructor_source_map, constructor_registers) =
            constructor.finish().unwrap();
        let entry_metadata = FunctionMetadata {
            layout: FunctionLayout {
                register_count: entry_registers,
                ..FunctionLayout::default()
            },
            source_map: entry_source_map,
            ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
        };
        let constructor_metadata = FunctionMetadata {
            layout: FunctionLayout {
                register_count: constructor_registers,
                argument_count: 1,
                ..FunctionLayout::default()
            },
            source_map: constructor_source_map,
            ..FunctionMetadata::new(FunctionKind::Ordinary, FunctionLayout::default())
        };
        CompiledModule::new(
            Arc::from("construct"),
            Vec::new(),
            vec![Arc::from("value")],
            vec![
                CompiledFunctionTemplate::new(FunctionId::new(0), entry_bytecode, entry_metadata),
                CompiledFunctionTemplate::new(
                    FunctionId::new(1),
                    constructor_bytecode,
                    constructor_metadata,
                ),
            ],
            FunctionId::new(0),
        )
        .unwrap()
    }

    /// Builds one default constructor, constructs its receiver, then checks the real prototype link.
    fn instanceof_module() -> CompiledModule {
        let span = SourceSpan { start: 0, end: 1 };
        let mut entry = BytecodeBuilder::with_capacity(4, 0);
        entry.emit(Opcode::CreateClosure, &[0, 1], span).unwrap();
        entry.emit(Opcode::Construct, &[1, 0, 0], span).unwrap();
        entry.emit(Opcode::InstanceOf, &[2, 1, 0], span).unwrap();
        entry.emit(Opcode::Return, &[2], span).unwrap();
        let (entry_bytecode, entry_source_map, entry_registers) = entry.finish().unwrap();
        let mut constructor = BytecodeBuilder::with_capacity(2, 0);
        constructor.emit(Opcode::LoadUndefined, &[0], span).unwrap();
        constructor.emit(Opcode::Return, &[0], span).unwrap();
        let (constructor_bytecode, constructor_source_map, constructor_registers) =
            constructor.finish().unwrap();
        CompiledModule::new(
            Arc::from("new Constructor() instanceof Constructor"),
            Vec::new(),
            Vec::new(),
            vec![
                CompiledFunctionTemplate::new(
                    FunctionId::new(0),
                    entry_bytecode,
                    FunctionMetadata {
                        layout: FunctionLayout {
                            register_count: entry_registers,
                            ..FunctionLayout::default()
                        },
                        source_map: entry_source_map,
                        ..FunctionMetadata::new(FunctionKind::Script, FunctionLayout::default())
                    },
                ),
                CompiledFunctionTemplate::new(
                    FunctionId::new(1),
                    constructor_bytecode,
                    FunctionMetadata {
                        layout: FunctionLayout {
                            register_count: constructor_registers,
                            ..FunctionLayout::default()
                        },
                        source_map: constructor_source_map,
                        ..FunctionMetadata::new(FunctionKind::Ordinary, FunctionLayout::default())
                    },
                ),
            ],
            FunctionId::new(0),
        )
        .unwrap()
    }

    /// Builds the smallest executable module carrying a chosen entry-state layout contract.
    fn state_module(kind: FunctionKind, layout: FunctionLayout) -> CompiledModule {
        let mut words = encode_instruction(Opcode::LoadUndefined, &[0]).unwrap();
        words.extend(encode_instruction(Opcode::Return, &[0]).unwrap());
        CompiledModule::new(
            Arc::from("state"),
            Vec::new(),
            Vec::new(),
            vec![CompiledFunctionTemplate::new(
                FunctionId::new(0),
                Bytecode::from_words(words),
                FunctionMetadata::new(kind, layout),
            )],
            FunctionId::new(0),
        )
        .unwrap()
    }
}
