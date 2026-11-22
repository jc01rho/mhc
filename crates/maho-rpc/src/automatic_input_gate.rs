use maho_core::agent_session::AgentSession;
pub fn hold_automatic_input(session: &AgentSession, message: &str, interactive_user: bool) -> bool {
    !interactive_user && session.note_automatic_input(message)
}

/// Whether `hold_automatic_input` would keep this connection's input as a notice right now, without storing anything.
pub fn automatic_input_would_be_held(session: &AgentSession, interactive_user: bool) -> bool {
    !interactive_user && session.automatic_input_hold_live()
}
