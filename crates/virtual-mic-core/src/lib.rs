//! User-mode Rust client API for streaming PCM into the Virtual Mic capture endpoint.
#![doc = include_str!("../../../README.md")]
#![cfg(windows)]

mod device;
mod format;
mod resampler;
mod writer;

pub use device::{Error, Result, Status, VirtualMic};
pub use format::{AudioFormat, SUPPORTED_FORMATS};
pub use writer::ConvertingWriter;
