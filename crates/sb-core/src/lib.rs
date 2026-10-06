//! Domain types and traits for second-brain. This crate performs no I/O.

pub mod budget;
pub mod clock;
pub mod coverage;
pub mod document;
pub mod kinds;
pub mod model;
pub mod notify;
pub mod search;
pub mod source;
pub mod summarizer;
pub mod util;

pub use kinds::*;
pub use model::*;
