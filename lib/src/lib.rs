//! 编程工作区：代码、终端与 AI 助手
//! 
//! This crate is the stable integration boundary for the host and standalone shell.
pub const TOOL_NAME: &str = "roc_desk-workspace";
pub const TOOL_DESCRIPTION: &str = "编程工作区：代码、终端与 AI 助手";

/// Returns the user-visible metadata used by the standalone shell and host launcher.
pub fn tool_info() -> (&'static str, &'static str) {
    (TOOL_NAME, TOOL_DESCRIPTION)
}
