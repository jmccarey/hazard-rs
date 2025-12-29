#[cfg(feature = "loom")]
pub use loom::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

#[cfg(feature = "loom")]
pub use loom::sync::Mutex;

#[cfg(not(feature = "loom"))]
pub use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

#[cfg(not(feature = "loom"))]
pub use std::sync::Mutex;

