//! The interfaces every Flow lane codes against.
//!
//! - [`model`]: the workflow as the web stores it (the graph kept as JSON so
//!   export and import round-trip), the typed [`model::Graph`] the walk
//!   reads, node type ids and the type groups the walk treats specially,
//!   the stored workflow and execution rows.
//! - [`node`]: [`node::FlowNode`] (one per node type), [`node::NodeCtx`]
//!   (client, variables, log, cancel, clock, workflow id, the web's
//!   accessors) and [`node::NodeOutcome`] (the web's result dict, from which
//!   the walk picks the next nodes).
//! - [`vars`]: `{{...}}` interpolation, built-ins and the variable store
//!   (the web's `WorkflowContext`).
//! - [`resolve`]: order fields read without silent defaults (the web's
//!   `RuntimeOrderResolver`).
//! - [`client`]: [`client::FlowClient`], the web's `FlowOpenAlgoClient` over
//!   the in-process `/api/v1` dispatch, and a scripted fake for tests.
//! - [`env`]: what nodes read besides the client (symbol master, strategy
//!   book, live feed, outbound HTTP, trading calendar), as traits the node
//!   lanes implement and tests fake.
//! - [`subscriptions`]: the feed subscriptions each workflow holds.
//! - [`time`]: market time (IST), `parse_time_string`, the wait bounds.
//! - [`triggers`]: what the core asks of the scheduler and monitors.
//! - [`pyfmt`]: Python's text forms of values (`str`, `repr`, `float`,
//!   `json.dumps`), which the web's messages and interpolation use; shared
//!   by the validator and every node.

pub mod client;
pub mod env;
pub mod model;
pub mod node;
pub mod pyfmt;
pub mod resolve;
pub mod subscriptions;
pub mod time;
pub mod triggers;
pub mod vars;
