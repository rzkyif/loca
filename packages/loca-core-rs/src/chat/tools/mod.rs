//! Tool selection and tool-call dispatch for one turn. [`selection`] ranks the
//! catalog to decide which tools a hop is offered; [`dispatch`] runs one call
//! against the sandboxed worker. They share only
//! [`ToolHost`](super::deps::ToolHost).

pub mod dispatch;
pub mod selection;

pub use dispatch::{Dispatcher, InFlight, InFlightEntry};
pub use selection::{build_tool_list, tool_exposable, SelectionInputs, ToolSelection};
