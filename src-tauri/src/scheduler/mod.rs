//! Scheduler module for OpenAlgo Desktop
//!
//! Handles scheduled tasks including:
//! - Auto-logout at 3:00 AM IST (broker compliance)
//! - Future: Strategy scheduling, market timings

mod auto_logout;

pub use auto_logout::AutoLogoutScheduler;
