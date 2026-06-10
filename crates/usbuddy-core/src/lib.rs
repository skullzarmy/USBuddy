pub mod atomic;
pub mod catalog;
pub mod download;
pub mod engine;
pub mod error;
pub mod gguf;
pub mod hash;
pub mod layout;
pub mod license;
pub mod platform;
pub mod ram;
pub mod release;

pub use error::{Result, UsbBuddyError};

pub fn compiled_version() -> &'static str {
    option_env!("USBUDDY_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"))
}
