//! Tool selection and tool-call dispatch for one turn.
//!
//! Two halves with different shapes. [`selection`] decides WHICH tools a hop is
//! offered, which is a ranking problem over the catalog and the relevance index.
//! [`dispatch`] runs ONE call the model asked for, which is a state machine over
//! a sandboxed worker and its prompts. They share nothing but the
//! [`ToolHost`](super::deps::ToolHost) trait.

pub mod dispatch;
pub mod selection;

pub use dispatch::{Dispatcher, InFlight, InFlightEntry};
pub use selection::{build_tool_list, tool_exposable, SelectionInputs, ToolSelection};
