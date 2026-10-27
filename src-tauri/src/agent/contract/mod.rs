//! The interfaces every Agent lane codes against.
//!
//! - [`frames`]: the stream frames, wire-exact with the web's `frames.py`.
//! - [`tool`]: [`tool::AgentTool`], [`tool::ToolCtx`], [`tool::ToolOutcome`],
//!   surfaces, capabilities and tool groups (the web's `tools/base.py` and
//!   `ToolContext`).
//! - [`json`]: tool results within the 12,000 character cap.
//! - [`model`]: [`model::ResolvedModel`] and the [`model::ModelResolver`] the
//!   config lane implements.
//! - [`viz`]: the side channel a rendering tool's frame travels on.
//! - [`approval`]: the hook the orders lane hangs approval decisions on.
//! - [`policy`]: the trader's Agent settings a run reads, and the prompt
//!   sections other parts add.

pub mod approval;
pub mod frames;
pub mod json;
pub mod model;
pub mod policy;
pub mod tool;
pub mod viz;
