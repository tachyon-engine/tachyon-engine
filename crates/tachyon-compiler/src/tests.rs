use std::sync::Arc;

use proptest::prelude::*;

use super::*;

fn source(media_type: MediaType, text: &str) -> SourceText {
    SourceText::new(
        SourceId::new(7),
        SourceName::new("embedded-input"),
        media_type,
        Arc::from(text),
    )
}

mod emission;
mod frontend;
mod structured;
