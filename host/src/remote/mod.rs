//! Reaching this computer from other devices: identity keys, wire crypto, the
//! E2E channel, the Codync cloud and its relay socket, and push. And the other way:
//! Hermes gateways on the user's other computers, which remote bots run on.

pub mod channel;
pub mod cloud;
mod cloud_url;
pub mod crypto;
pub mod hermes;
pub mod identity;
pub mod push;
pub mod relay;
