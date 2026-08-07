//! DisplayNames provider boundary without importing a second ICU4X data stack.

use tachyon_vm::{
    HostProviderError, IntlDisplayNamesBackend, IntlDisplayNamesCreation, IntlDisplayNamesRequest,
    IntlDisplayNamesResolved, IntlDisplayNamesType, IntlLocaleMatcher,
};

/// Minimal owned backend used until ICU4X 2.x exposes stable display-name constructors.
struct Icu4xDisplayNamesBackend {
    display_type: IntlDisplayNamesType,
}

impl IntlDisplayNamesBackend for Icu4xDisplayNamesBackend {
    #[inline]
    fn display_name(&self, code: &str) -> Result<Option<Box<[u16]>>, HostProviderError> {
        let supported = match self.display_type {
            IntlDisplayNamesType::Calendar => super::supported_values::CALENDARS
                .binary_search(&code)
                .is_ok(),
            IntlDisplayNamesType::Currency => super::supported_values::CURRENCIES
                .binary_search(&code)
                .is_ok(),
            _ => false,
        };
        Ok(supported.then(|| code.encode_utf16().collect()))
    }

    #[inline]
    fn external_memory_bytes(&self) -> usize {
        0
    }
}

/// Resolves locale and freezes scalar options behind the provider-neutral ABI.
pub(super) fn create(
    default_locale: &str,
    request: IntlDisplayNamesRequest,
) -> Result<IntlDisplayNamesCreation, HostProviderError> {
    let locale = request
        .locales
        .first()
        .map_or(default_locale, Box::as_ref)
        .split("-u-")
        .next()
        .unwrap_or(default_locale)
        .into();
    Ok(IntlDisplayNamesCreation {
        resolved: IntlDisplayNamesResolved {
            locale,
            style: request.style,
            display_type: request.display_type,
            fallback: request.fallback,
            language_display: request.language_display,
        },
        backend: Box::new(Icu4xDisplayNamesBackend {
            display_type: request.display_type,
        }),
    })
}

/// DisplayNames shares the adapter's broad locale availability boundary.
pub(super) fn supported_locales(
    locales: &[Box<str>],
    matcher: IntlLocaleMatcher,
) -> Box<[Box<str>]> {
    super::number_format::supported_locales(locales, matcher)
}
