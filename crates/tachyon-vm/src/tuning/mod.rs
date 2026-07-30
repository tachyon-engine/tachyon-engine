//! Centralized VM tuning families; resource limits remain typed host configuration.

pub(crate) mod arrays;
pub(crate) mod bigints;
pub(crate) mod buffers;
pub(crate) mod collections;
pub(crate) mod dispatch;
pub(crate) mod json;
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "module tuning is consumed when the M8.4 graph is wired into the isolate"
    )
)]
pub(crate) mod modules;
pub(crate) mod numbers;
pub(crate) mod objects;
pub(crate) mod promises;
pub(crate) mod realms;
pub(crate) mod signals;
pub(crate) mod strings;
