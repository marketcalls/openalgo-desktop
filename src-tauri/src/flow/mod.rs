//! Flow, the visual workflow builder (`/flow`): the web's
//! `services/flow_*.py` in Rust.
//!
//! | Module | What lives there |
//! | --- | --- |
//! | `node_contracts` | statuses, expiry and strike vocabularies, default product, expiry selection |
//! | `validator` | the web's workflow validator, rule for rule |

pub mod node_contracts;
pub mod validator;
