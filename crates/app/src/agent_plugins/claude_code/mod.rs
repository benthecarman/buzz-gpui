//! Claude Code agents, buzz-app's `src/agent-plugins/claude-code`: an
//! Agents2 type whose agents are Claude Code sessions on the host that holds
//! them. Each conversation the agent is in is one continuing session;
//! Claude answers with the in-process Buzz tools.

pub mod claude;
pub mod prompt;
pub mod sessions;
pub mod setup;
