//! Resumable `Intl.Locale` construction and ordered option processing.

use super::super::*;
use crate::runtime::fiber::IntlLocaleStage;

const UNDEFINED: Value = Value::from_immediate(Immediate::Undefined);

/// GC-managed Locale constructor state retained across every observable callback.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PendingIntlLocale {
    new_target: Value,
    options: Value,
    tag: Value,
    language: Value,
    script: Value,
    region: Value,
    variants: Value,
    calendar: Value,
    collation: Value,
    hour_cycle: Value,
    case_first: Value,
    numbering_system: Value,
    numeric: Option<bool>,
    stage: IntlLocaleStage,
}

impl Trace for PendingIntlLocale {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.new_target.trace(tracer);
        self.options.trace(tracer);
        self.tag.trace(tracer);
        self.language.trace(tracer);
        self.script.trace(tracer);
        self.region.trace(tracer);
        self.variants.trace(tracer);
        self.calendar.trace(tracer);
        self.collation.trace(tracer);
        self.hour_cycle.trace(tracer);
        self.case_first.trace(tracer);
        self.numbering_system.trace(tracer);
    }
}

struct PendingIntlLocaleRoots<'a> {
    vm: VmRoots<'a>,
    pending: PendingIntlLocale,
}

impl Trace for PendingIntlLocaleRoots<'_> {
    #[inline(always)]
    fn trace(&mut self, tracer: &mut dyn Tracer) {
        self.vm.trace(tracer);
        self.pending.trace(tracer);
    }
}

impl PendingIntlLocale {
    #[inline]
    fn new(new_target: Value, options: Value) -> Self {
        Self {
            new_target,
            options,
            tag: UNDEFINED,
            language: UNDEFINED,
            script: UNDEFINED,
            region: UNDEFINED,
            variants: UNDEFINED,
            calendar: UNDEFINED,
            collation: UNDEFINED,
            hour_cycle: UNDEFINED,
            case_first: UNDEFINED,
            numbering_system: UNDEFINED,
            numeric: None,
            stage: IntlLocaleStage::Language,
        }
    }
}

impl Isolate {
    /// Starts construction and performs the tag conversion before touching the options object.
    pub(crate) fn begin_intl_locale_constructor(
        &mut self,
        site: &CallSite,
    ) -> Result<(), ExecutionError> {
        if !self.is_object_value(site.new_target) {
            return Err(ExecutionError::NonConstructor(site.callee));
        }
        let tag = self.call_argument(site, 0)?.unwrap_or(UNDEFINED);
        let options = self.call_argument(site, 1)?.unwrap_or(UNDEFINED);
        if !self.is_string_value(tag) && !self.is_object_value(tag) {
            return Err(ExecutionError::InvalidLocaleListElement(tag));
        }
        let pending =
            self.allocate_pending_intl_locale(PendingIntlLocale::new(site.new_target, options))?;
        let native_site = Self::native_site(site);
        if let Ok(locale) = self.intl_locale_reference(tag) {
            let tag = self.intl_locale_tag(locale)?;
            return self.resume_intl_locale_tag(native_site, pending, tag);
        }
        if self.is_object_value(tag) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::IntlLocaleTag,
                native_site.caller_base,
                native_site.destination,
                Value::from_heap_ref(pending.raw()),
                tag,
                native_site.call_site,
            );
        }
        let tag = if self.is_string_value(tag) {
            tag
        } else {
            self.primitive_to_string_value(tag)?
        };
        self.resume_intl_locale_tag(native_site, pending, tag)
    }

    /// Canonicalizes the input once, roots it in pending state, then begins ordered option access.
    pub(crate) fn resume_intl_locale_tag(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlLocale>,
        tag: Value,
    ) -> Result<(), ExecutionError> {
        let canonical = self.canonicalize_intl_locale_text(tag)?;
        let (tag, retained) = self.allocate_runtime_string_retaining(
            JsString::try_from_str(&canonical).map_err(ExecutionError::PropertyKeyString)?,
            Value::from_heap_ref(state.raw()),
        )?;
        let state = self.pending_intl_locale_reference(retained)?;
        self.set_pending_intl_locale_value(state, IntlLocaleValueSlot::Tag, tag)?;
        let snapshot = self.pending_intl_locale_snapshot(state)?;
        if snapshot.options == UNDEFINED {
            return self.finish_pending_intl_locale(site, state);
        }
        self.write(site.caller_base, site.destination, retained)?;
        let options = self.coerce_to_object(snapshot.options)?;
        let state =
            self.pending_intl_locale_reference(self.read(site.caller_base, site.destination)?)?;
        self.set_pending_intl_locale_value(state, IntlLocaleValueSlot::Options, options)?;
        self.dispatch_intl_locale_option_get(site, state, IntlLocaleStage::Language)
    }

    /// Resumes an option Get, applying ToBoolean directly or dispatching string conversion.
    pub(crate) fn resume_pending_intl_locale(
        &mut self,
        continuation: NativeContinuation,
        stage: IntlLocaleStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let state = self.pending_intl_locale_reference(continuation.first())?;
        if value == UNDEFINED {
            return self.advance_intl_locale_option(continuation.site(), state, stage);
        }
        if stage == IntlLocaleStage::Numeric {
            let numeric = self.is_truthy_value(value)?;
            self.update_pending_intl_locale(state, |pending| pending.numeric = Some(numeric))?;
            return self.advance_intl_locale_option(continuation.site(), state, stage);
        }
        if self.is_object_value(value) {
            return self.dispatch_object_primitive_conversion(
                ConversionConsumer::IntlLocaleOption,
                continuation.site().caller_base,
                continuation.site().destination,
                Value::from_heap_ref(state.raw()),
                value,
                continuation.site().call_site,
            );
        }
        let string = self.primitive_to_string_value(value)?;
        self.store_intl_locale_string_option(continuation.site(), state, stage, string)
    }

    /// Validates and stores an already stringified option without retaining Rust-owned strings.
    pub(crate) fn resume_intl_locale_option_primitive(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlLocale>,
        primitive: Value,
    ) -> Result<(), ExecutionError> {
        let string = self.primitive_to_string_value(primitive)?;
        let stage = self.pending_intl_locale_snapshot(state)?.stage;
        self.store_intl_locale_string_option(site, state, stage, string)
    }

    fn store_intl_locale_string_option(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlLocale>,
        stage: IntlLocaleStage,
        string: Value,
    ) -> Result<(), ExecutionError> {
        let text = self
            .string_value_to_ascii(string)
            .map_err(|_| ExecutionError::InvalidLanguageTag)?;
        if !is_valid_intl_locale_option(stage, &text) {
            return Err(ExecutionError::InvalidLanguageTag);
        }
        let slot = IntlLocaleValueSlot::from_stage(stage)
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.set_pending_intl_locale_value(state, slot, string)?;
        self.advance_intl_locale_option(site, state, stage)
    }

    #[inline]
    fn advance_intl_locale_option(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlLocale>,
        stage: IntlLocaleStage,
    ) -> Result<(), ExecutionError> {
        let Some(next) = next_intl_locale_stage(stage) else {
            return self.finish_pending_intl_locale(site, state);
        };
        self.dispatch_intl_locale_option_get(site, state, next)
    }

    /// Performs one accessor/Proxy-aware Get under a Locale-specific continuation.
    fn dispatch_intl_locale_option_get(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlLocale>,
        stage: IntlLocaleStage,
    ) -> Result<(), ExecutionError> {
        self.update_pending_intl_locale(state, |pending| pending.stage = stage)?;
        let snapshot = self.pending_intl_locale_snapshot(state)?;
        let key = self
            .intern_intrinsic_name(intl_locale_option_name(stage))?
            .into();
        let continuation = NativeContinuation::intl_locale(
            site,
            stage,
            Value::from_heap_ref(state.raw()),
            snapshot.options,
        );
        match self.resolve_property_read_until_proxy(snapshot.options, key)? {
            PropertyReadResolution::Read(PropertyRead::Missing) => {
                self.resume_pending_intl_locale(continuation, stage, UNDEFINED)
            }
            PropertyReadResolution::Read(PropertyRead::Data(value)) => {
                self.resume_pending_intl_locale(continuation, stage, value)
            }
            PropertyReadResolution::Read(PropertyRead::Accessor(getter)) if getter == UNDEFINED => {
                self.resume_pending_intl_locale(continuation, stage, UNDEFINED)
            }
            PropertyReadResolution::Read(PropertyRead::Accessor(getter)) => self
                .dispatch_property_callback(
                    NativeContinuation::intl_locale_property_get(
                        site,
                        Value::from_heap_ref(state.raw()),
                        snapshot.options,
                    ),
                    getter,
                )
                .map(|_| ()),
            PropertyReadResolution::Proxy(_) => {
                self.dispatch_intl_locale_nested(continuation, |isolate| {
                    isolate
                        .dispatch_proxy_aware_property_read(
                            site,
                            snapshot.options,
                            snapshot.options,
                            key,
                        )
                        .map(|_| ())
                })
            }
        }
    }

    /// Converts traced option values into an owned provider request and allocates the branded result.
    fn finish_pending_intl_locale(
        &mut self,
        site: NativeContinuationSite,
        state: GcRef<PendingIntlLocale>,
    ) -> Result<(), ExecutionError> {
        let snapshot = self.pending_intl_locale_snapshot(state)?;
        let request = IntlLocaleRequest {
            tag: self.intl_locale_owned_string(snapshot.tag)?,
            language: self.optional_intl_locale_string(snapshot.language)?,
            script: self.optional_intl_locale_string(snapshot.script)?,
            region: self.optional_intl_locale_string(snapshot.region)?,
            variants: self.optional_intl_locale_string(snapshot.variants)?,
            calendar: self.optional_intl_locale_string(snapshot.calendar)?,
            collation: self.optional_intl_locale_string(snapshot.collation)?,
            hour_cycle: self.optional_intl_locale_string(snapshot.hour_cycle)?,
            case_first: self.optional_intl_locale_string(snapshot.case_first)?,
            numeric: snapshot.numeric,
            numbering_system: self.optional_intl_locale_string(snapshot.numbering_system)?,
        };
        let canonical = self
            .host_providers
            .intl_mut()
            .ok_or(ExecutionError::MissingIntlProvider)?
            .create_locale(request)
            .map_err(ExecutionError::IntlProvider)?
            .ok_or(ExecutionError::InvalidLanguageTag)?;
        let (value, retained) = self.allocate_runtime_string_retaining(
            JsString::try_from_str(&canonical).map_err(ExecutionError::PropertyKeyString)?,
            Value::from_heap_ref(state.raw()),
        )?;
        let state = self.pending_intl_locale_reference(retained)?;
        let new_target = self.pending_intl_locale_snapshot(state)?.new_target;
        let prototype_atom = self.prototype_atom()?;
        let default_prototype = self
            .realm
            .intl_locale_prototype
            .expect("Intl.Locale prototype initializes before construction");
        let prototype = self
            .constructor_prototype_value(new_target, prototype_atom)?
            .filter(|value| self.is_object_value(*value))
            .or_else(|| {
                self.realm_for_callable(new_target).ok().and_then(|realm| {
                    self.realm_intrinsic_prototype(realm, IntrinsicPrototypeKind::IntlLocale)
                })
            })
            .unwrap_or(default_prototype);
        let locale = self.allocate_intl_locale_object(value, prototype, AllocationSpace::Young)?;
        self.write(site.caller_base, site.destination, locale)
    }

    fn intl_locale_owned_string(&mut self, value: Value) -> Result<Box<str>, ExecutionError> {
        self.string_value_to_ascii(value)
            .map(String::into_boxed_str)
    }

    fn optional_intl_locale_string(
        &mut self,
        value: Value,
    ) -> Result<Option<Box<str>>, ExecutionError> {
        (value != UNDEFINED)
            .then(|| self.intl_locale_owned_string(value))
            .transpose()
    }

    fn allocate_pending_intl_locale(
        &mut self,
        pending: PendingIntlLocale,
    ) -> Result<GcRef<PendingIntlLocale>, ExecutionError> {
        let mut roots = PendingIntlLocaleRoots {
            vm: VmRoots {
                fiber: &mut self.fiber,
                suspended_fibers: &mut self.suspended_fibers,
                finalization_jobs: &mut self.finalization_jobs,
                promise_jobs: &mut self.promise_jobs,
                realm: &mut self.realm,
                inactive_realms: &mut self.inactive_realms,
                loaded_code: &mut self.loaded_code,
                module_graph: &mut self.module_graph,
            },
            pending,
        };
        self.heap
            .try_allocate_with_gc(
                self.types.pending_intl_locale,
                0,
                0,
                roots.pending,
                AllocationSpace::Young,
                &mut roots,
            )
            .map_err(ExecutionError::HeapAllocation)
    }

    pub(crate) fn pending_intl_locale_reference(
        &self,
        value: Value,
    ) -> Result<GcRef<PendingIntlLocale>, ExecutionError> {
        let raw = value
            .as_heap_ref()
            .ok_or(ExecutionError::MissingNativeContinuation)?;
        self.heap
            .checked_reference(raw, self.types.pending_intl_locale)
            .map_err(ExecutionError::HeapReference)
    }

    fn pending_intl_locale_snapshot(
        &mut self,
        state: GcRef<PendingIntlLocale>,
    ) -> Result<PendingIntlLocale, ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                no_gc
                    .borrow(state, self.types.pending_intl_locale)
                    .copied()
                    .map_err(ExecutionError::NoGcBorrow)
            })
        })
    }

    pub(crate) fn pending_intl_locale_stage(
        &mut self,
        state: GcRef<PendingIntlLocale>,
    ) -> Result<IntlLocaleStage, ExecutionError> {
        self.pending_intl_locale_snapshot(state)
            .map(|pending| pending.stage)
    }

    fn update_pending_intl_locale(
        &mut self,
        state: GcRef<PendingIntlLocale>,
        update: impl FnOnce(&mut PendingIntlLocale),
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                update(
                    no_gc
                        .borrow_mut(state, self.types.pending_intl_locale)
                        .map_err(ExecutionError::NoGcBorrow)?,
                );
                Ok(())
            })
        })
    }

    fn set_pending_intl_locale_value(
        &mut self,
        state: GcRef<PendingIntlLocale>,
        slot: IntlLocaleValueSlot,
        value: Value,
    ) -> Result<(), ExecutionError> {
        self.heap.with_running_scope(|scope| {
            let state = scope.root(state).map_err(ExecutionError::Root)?;
            scope.with_no_gc_scope(|no_gc| {
                let pending = no_gc
                    .borrow_mut(state, self.types.pending_intl_locale)
                    .map_err(ExecutionError::NoGcBorrow)?;
                match slot {
                    IntlLocaleValueSlot::Options => pending.options = value,
                    IntlLocaleValueSlot::Tag => pending.tag = value,
                    IntlLocaleValueSlot::Language => pending.language = value,
                    IntlLocaleValueSlot::Script => pending.script = value,
                    IntlLocaleValueSlot::Region => pending.region = value,
                    IntlLocaleValueSlot::Variants => pending.variants = value,
                    IntlLocaleValueSlot::Calendar => pending.calendar = value,
                    IntlLocaleValueSlot::Collation => pending.collation = value,
                    IntlLocaleValueSlot::HourCycle => pending.hour_cycle = value,
                    IntlLocaleValueSlot::CaseFirst => pending.case_first = value,
                    IntlLocaleValueSlot::NumberingSystem => pending.numbering_system = value,
                }
                Ok::<(), ExecutionError>(())
            })?;
            scope
                .write_value_barrier(state, value)
                .map_err(ExecutionError::HeapReference)
                .map(|_| ())
        })
    }

    /// Drains synchronous Proxy reads without recursive Rust continuation growth.
    fn dispatch_intl_locale_nested(
        &mut self,
        continuation: NativeContinuation,
        operation: impl FnOnce(&mut Self) -> Result<(), ExecutionError>,
    ) -> Result<(), ExecutionError> {
        self.fiber
            .completions
            .push_native(continuation)
            .map_err(Self::completion_stack_error)?;
        let frame_depth = self.fiber.frames.len();
        if let Err(error) = operation(self) {
            self.pop_native_continuation()?;
            return Err(error);
        }
        if self.fiber.frames.len() != frame_depth {
            return Ok(());
        }
        let continuation = self.pop_native_continuation()?;
        let value = self.read(
            continuation.site().caller_base,
            continuation.site().destination,
        )?;
        let NativeContinuationKind::IntlLocale(stage) = continuation.kind() else {
            return Err(ExecutionError::MissingNativeContinuation);
        };
        self.resume_pending_intl_locale(continuation, stage, value)
    }
}

#[derive(Clone, Copy)]
enum IntlLocaleValueSlot {
    Options,
    Tag,
    Language,
    Script,
    Region,
    Variants,
    Calendar,
    Collation,
    HourCycle,
    CaseFirst,
    NumberingSystem,
}

impl IntlLocaleValueSlot {
    #[inline]
    const fn from_stage(stage: IntlLocaleStage) -> Option<Self> {
        Some(match stage {
            IntlLocaleStage::Language => Self::Language,
            IntlLocaleStage::Script => Self::Script,
            IntlLocaleStage::Region => Self::Region,
            IntlLocaleStage::Variants => Self::Variants,
            IntlLocaleStage::Calendar => Self::Calendar,
            IntlLocaleStage::Collation => Self::Collation,
            IntlLocaleStage::HourCycle => Self::HourCycle,
            IntlLocaleStage::CaseFirst => Self::CaseFirst,
            IntlLocaleStage::Numeric => return None,
            IntlLocaleStage::NumberingSystem => Self::NumberingSystem,
        })
    }
}

#[inline(always)]
const fn next_intl_locale_stage(stage: IntlLocaleStage) -> Option<IntlLocaleStage> {
    Some(match stage {
        IntlLocaleStage::Language => IntlLocaleStage::Script,
        IntlLocaleStage::Script => IntlLocaleStage::Region,
        IntlLocaleStage::Region => IntlLocaleStage::Variants,
        IntlLocaleStage::Variants => IntlLocaleStage::Calendar,
        IntlLocaleStage::Calendar => IntlLocaleStage::Collation,
        IntlLocaleStage::Collation => IntlLocaleStage::HourCycle,
        IntlLocaleStage::HourCycle => IntlLocaleStage::CaseFirst,
        IntlLocaleStage::CaseFirst => IntlLocaleStage::Numeric,
        IntlLocaleStage::Numeric => IntlLocaleStage::NumberingSystem,
        IntlLocaleStage::NumberingSystem => return None,
    })
}

#[inline(always)]
const fn intl_locale_option_name(stage: IntlLocaleStage) -> &'static [u8] {
    match stage {
        IntlLocaleStage::Language => b"language",
        IntlLocaleStage::Script => b"script",
        IntlLocaleStage::Region => b"region",
        IntlLocaleStage::Variants => b"variants",
        IntlLocaleStage::Calendar => b"calendar",
        IntlLocaleStage::Collation => b"collation",
        IntlLocaleStage::HourCycle => b"hourCycle",
        IntlLocaleStage::CaseFirst => b"caseFirst",
        IntlLocaleStage::Numeric => b"numeric",
        IntlLocaleStage::NumberingSystem => b"numberingSystem",
    }
}

fn is_valid_intl_locale_option(stage: IntlLocaleStage, text: &str) -> bool {
    match stage {
        IntlLocaleStage::Language => {
            matches!(text.len(), 2..=3 | 5..=8)
                && text.bytes().all(|byte| byte.is_ascii_alphabetic())
        }
        IntlLocaleStage::Script => {
            text.len() == 4 && text.bytes().all(|byte| byte.is_ascii_alphabetic())
        }
        IntlLocaleStage::Region => {
            (text.len() == 2 && text.bytes().all(|byte| byte.is_ascii_alphabetic()))
                || (text.len() == 3 && text.bytes().all(|byte| byte.is_ascii_digit()))
        }
        IntlLocaleStage::Variants => is_valid_locale_variants(text),
        IntlLocaleStage::Calendar
        | IntlLocaleStage::Collation
        | IntlLocaleStage::NumberingSystem => is_valid_unicode_locale_type(text),
        IntlLocaleStage::HourCycle => matches!(text, "h11" | "h12" | "h23" | "h24"),
        IntlLocaleStage::CaseFirst => matches!(text, "upper" | "lower" | "false"),
        IntlLocaleStage::Numeric => true,
    }
}

fn is_valid_unicode_locale_type(text: &str) -> bool {
    !text.is_empty()
        && text.split('-').all(|part| {
            (3..=8).contains(&part.len()) && part.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
}

/// Validates canonicalizable variant sequences without allocating a duplicate-detection set.
fn is_valid_locale_variants(text: &str) -> bool {
    if text.is_empty() {
        return false;
    }
    for (index, part) in text.split('-').enumerate() {
        let valid = ((5..=8).contains(&part.len())
            && part.bytes().all(|byte| byte.is_ascii_alphanumeric()))
            || (part.len() == 4
                && part.as_bytes()[0].is_ascii_digit()
                && part.bytes().all(|byte| byte.is_ascii_alphanumeric()));
        if !valid {
            return false;
        }
        if text
            .split('-')
            .take(index)
            .any(|previous| previous.eq_ignore_ascii_case(part))
        {
            return false;
        }
    }
    true
}
