//! The agentd client: the tailnet transport that replaces mhc-relayd for a device whose `relayd.json` peer carries an
//! `agentd` object (address, port, tokenFile).
//!
//! One TCP connection per peer carries LF-delimited JSON frames (`frame`), the link owns the handshake, the idempotent
//! resend after a reconnect and the per-stream seq/ack/resume (`link`), and the ops of the contract appear here as the
//! account surface mhc already reads: `ports` (listing, locate, lookup gate, command, open/fork, follow, attach),
//! `delivery` (`mhc.delivery.push`/`mhc.delivery.outcome`) and `peer_config` (which devices speak agentd at all).
//!
//! A device without an `agentd` entry is not configured here: it keeps the mhc-relayd path untouched.

pub mod delivery;
pub mod frame;
pub mod link;
pub mod peer_config;
pub mod ports;

pub use delivery::{
    AgentdDeliveryPort, DeliveryOutcome, DeliveryPushOutcome, DeliveryPushPort, push_outcome,
    sender_of,
};
pub use frame::{
    EventFrame, FrameError, RELAY_PROTOCOL, ResultFrame, ServerFrame, WelcomeFrame, parse_line,
};
pub use link::{AgentdError, AgentdLink};
pub use peer_config::{AgentdPeer, peer_map, peers_from_config, peers_in, read_token};
pub use ports::{
    AgentdAttach, AgentdClient, AgentdFollow, AgentdFollowPort, AgentdPorts, AgentdSessionHost,
    command_outcome, launch_of, opened_summary, outcome_of as command_error_outcome, reason_of,
    row_key_of, rows_of,
};
