//! Normative-optional ChainDateTimeFormat and UnwrapDateTimeFormat state machine.

use super::*;

impl Isolate {
    /// Applies ChainDateTimeFormat after the real branded formatter has been initialized.
    pub(super) fn begin_intl_date_time_format_chain(
        &mut self,
        site: NativeContinuationSite,
        receiver: Value,
        date_time_format: Value,
    ) -> Result<(), ExecutionError> {
        let constructor = self
            .realm
            .intl_date_time_format_constructor
            .expect("Intl.DateTimeFormat constructor initializes before legacy chaining");
        self.dispatch_intl_date_time_format_legacy(
            NativeContinuation::intl_date_time_format_legacy(
                site,
                IntlDateTimeFormatLegacyStage::ChainHasInstance,
                receiver,
                date_time_format,
            ),
            |isolate| isolate.begin_ordinary_has_instance(site, constructor, receiver),
        )
    }

    /// Starts OrdinaryHasInstance for one UnwrapDateTimeFormat consumer.
    pub(super) fn begin_intl_date_time_format_unwrap(
        &mut self,
        site: NativeContinuationSite,
        stage: IntlDateTimeFormatLegacyStage,
        receiver: Value,
    ) -> Result<(), ExecutionError> {
        if !self.is_object_value(receiver) {
            return Err(ExecutionError::IncompatibleIntlDateTimeFormatReceiver(
                receiver,
            ));
        }
        let constructor = self
            .realm
            .intl_date_time_format_constructor
            .expect("Intl.DateTimeFormat constructor initializes before unwrap");
        self.dispatch_intl_date_time_format_legacy(
            NativeContinuation::intl_date_time_format_legacy(
                site,
                stage,
                receiver,
                Value::from_immediate(Immediate::Undefined),
            ),
            |isolate| isolate.begin_ordinary_has_instance(site, constructor, receiver),
        )
    }

    /// Resumes ChainDateTimeFormat or UnwrapDateTimeFormat after one nested operation.
    pub(crate) fn resume_intl_date_time_format_legacy(
        &mut self,
        continuation: NativeContinuation,
        stage: IntlDateTimeFormatLegacyStage,
        value: Value,
    ) -> Result<(), ExecutionError> {
        let site = continuation.site();
        let receiver = continuation.first();
        match stage {
            IntlDateTimeFormatLegacyStage::ChainHasInstance => {
                let date_time_format = continuation.second();
                if !self.is_truthy_value(value)? {
                    return self.write(site.caller_base, site.destination, date_time_format);
                }
                self.define_intl_date_time_format_legacy_fallback(site, receiver, date_time_format)
            }
            IntlDateTimeFormatLegacyStage::ChainDefine => {
                self.write(site.caller_base, site.destination, receiver)
            }
            IntlDateTimeFormatLegacyStage::FormatHasInstance
            | IntlDateTimeFormatLegacyStage::ResolvedOptionsHasInstance => {
                if !self.is_truthy_value(value)? {
                    return Err(ExecutionError::IncompatibleIntlDateTimeFormatReceiver(
                        receiver,
                    ));
                }
                let get_stage = if stage == IntlDateTimeFormatLegacyStage::FormatHasInstance {
                    IntlDateTimeFormatLegacyStage::FormatFallbackGet
                } else {
                    IntlDateTimeFormatLegacyStage::ResolvedOptionsFallbackGet
                };
                self.dispatch_intl_date_time_format_fallback_get(site, get_stage, receiver)
            }
            IntlDateTimeFormatLegacyStage::FormatFallbackGet => {
                self.finish_intl_date_time_format_format_getter(site, value)
            }
            IntlDateTimeFormatLegacyStage::ResolvedOptionsFallbackGet => {
                self.finish_intl_date_time_format_resolved_options(site, value)
            }
        }
    }

    /// Defines the hidden fallback edge through ordinary or Proxy [[DefineOwnProperty]].
    fn define_intl_date_time_format_legacy_fallback(
        &mut self,
        site: NativeContinuationSite,
        receiver: Value,
        date_time_format: Value,
    ) -> Result<(), ExecutionError> {
        let key = self.intl_date_time_format_legacy_key()?;
        let descriptor = PropertyDescriptor::Data(DataPropertyDescriptor {
            value: Some(date_time_format),
            writable: Some(false),
            enumerable: Some(false),
            configurable: Some(false),
        });
        if !self.is_proxy_value(receiver) {
            self.define_property(receiver, key, descriptor)?;
            return self.write(site.caller_base, site.destination, receiver);
        }
        self.dispatch_intl_date_time_format_legacy(
            NativeContinuation::intl_date_time_format_legacy(
                site,
                IntlDateTimeFormatLegacyStage::ChainDefine,
                receiver,
                date_time_format,
            ),
            |isolate| {
                isolate.dispatch_proxy_define(
                    site,
                    receiver,
                    key,
                    descriptor,
                    ProxyDefineMode::Object,
                )
            },
        )
    }

    /// Performs the observable fallback-symbol Get required by UnwrapDateTimeFormat.
    fn dispatch_intl_date_time_format_fallback_get(
        &mut self,
        site: NativeContinuationSite,
        stage: IntlDateTimeFormatLegacyStage,
        receiver: Value,
    ) -> Result<(), ExecutionError> {
        let key = self.intl_date_time_format_legacy_key()?;
        self.dispatch_intl_date_time_format_legacy(
            NativeContinuation::intl_date_time_format_legacy(
                site,
                stage,
                receiver,
                Value::from_immediate(Immediate::Undefined),
            ),
            |isolate| isolate.dispatch_proxy_aware_property_read(site, receiver, receiver, key),
        )
    }

    /// Drains a synchronous nested MOP or leaves its typed parent below a JavaScript frame.
    fn dispatch_intl_date_time_format_legacy(
        &mut self,
        continuation: NativeContinuation,
        operation: impl FnOnce(&mut Self) -> Result<Option<RunOutcome>, ExecutionError>,
    ) -> Result<(), ExecutionError> {
        let completion_depth = self.fiber.completions.len();
        self.fiber
            .completions
            .push_native(continuation)
            .map_err(Self::completion_stack_error)?;
        let frame_depth = self.fiber.frames.len();
        let outcome = match operation(self) {
            Ok(outcome) => outcome,
            Err(error) => {
                if self.fiber.completions.len() > completion_depth {
                    self.pop_native_continuation()?;
                }
                return Err(error);
            }
        };
        if self.fiber.completions.len() == completion_depth
            || self.fiber.frames.len() != frame_depth
        {
            return Ok(());
        }
        debug_assert!(outcome.is_none());
        let continuation = self.pop_native_continuation()?;
        let value = self.read(
            continuation.site().caller_base,
            continuation.site().destination,
        )?;
        let NativeContinuationKind::IntlDateTimeFormatLegacy(stage) = continuation.kind() else {
            return Err(ExecutionError::MissingNativeContinuation);
        };
        self.resume_intl_date_time_format_legacy(continuation, stage, value)
    }

    /// Returns the per-Realm hidden Intl fallback Symbol as a property key.
    fn intl_date_time_format_legacy_key(&mut self) -> Result<PropertyKey, ExecutionError> {
        let symbol = self
            .realm
            .intl_legacy_constructed_symbol
            .expect("Intl fallback symbol initializes before DateTimeFormat use");
        self.property_key(symbol)
    }
}
