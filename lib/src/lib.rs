//! Coding workspace integration boundary.
pub const TOOL_NAME: &str = "roc_desk-workspace";
pub const TOOL_DESCRIPTION: &str = "编程工作区：代码、终端与 AI 助手";
pub use roc_desk_core::connection::{ConnectionKind, ConnectionProfile};
pub fn tool_info() -> (&'static str, &'static str) { (TOOL_NAME, TOOL_DESCRIPTION) }
