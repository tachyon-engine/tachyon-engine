use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct SourceId(u32);

impl SourceId {
    #[must_use]
    pub const fn new(index: u32) -> Self {
        Self(index)
    }

    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }
}

/// An opaque host-provided label for diagnostics; it is never interpreted as a filesystem path.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct SourceName(Arc<str>);

impl SourceName {
    #[must_use]
    pub fn new(name: impl Into<Arc<str>>) -> Self {
        Self(name.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MediaType {
    #[default]
    JavaScript,
    Jsx,
    TypeScript,
    Tsx,
    Mjs,
    Cjs,
    Mts,
    Cts,
}

/// Source text is caller-owned input retained by Arc, never obtained through a compiler filesystem API.
#[derive(Clone, Debug)]
pub struct SourceText {
    id: SourceId,
    name: SourceName,
    media_type: MediaType,
    text: Arc<str>,
}

impl SourceText {
    #[must_use]
    pub fn new(id: SourceId, name: SourceName, media_type: MediaType, text: Arc<str>) -> Self {
        Self {
            id,
            name,
            media_type,
            text,
        }
    }

    #[must_use]
    pub const fn id(&self) -> SourceId {
        self.id
    }

    #[must_use]
    pub fn name(&self) -> &SourceName {
        &self.name
    }

    #[must_use]
    pub const fn media_type(&self) -> MediaType {
        self.media_type
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub(crate) fn shared_text(&self) -> Arc<str> {
        Arc::clone(&self.text)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SourceMode {
    #[default]
    Auto,
    Script,
    Module,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CompileOptions {
    pub source_mode: SourceMode,
    /// Compiles Script root lexical bindings into an eval-local declarative environment.
    pub direct_eval: bool,
}
