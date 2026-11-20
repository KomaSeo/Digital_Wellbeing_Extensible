//! Responsibility: Serve the agent's API: the requests the Focus app and Claude Desktop send
//! (`user_api`), the tools Claude Desktop's MCP server offers over them (`assistant_tools`),
//! and the link between the task planner and the AI focus monitor (`focus_link`).
//!
//! This is the top of the agent: it uses the settings, the lockdown and the logins
//! (`machine`), the program updates (`system_care`) and the rules; nothing below uses it.

pub mod assistant_tools;
pub mod focus_link;
pub mod user_api;
