//! Setup steps for second-brain (ADR-0009, setup-and-scheduling.md): home
//! initialization, scheduler registration, skill install and environment
//! setup. Interactive prompting lives in the CLI; these are library functions
//! so a future GUI can reuse them.

pub mod env;
pub mod error;
pub mod home;
pub mod llm;
pub mod schedule;
pub mod skills;

pub use error::{Result, SetupError};
