//! Coordinator-free business messaging. Frontends bind authority, then use one engine.
pub mod codec;
pub mod lifecycle;
pub mod seat_store;
pub mod send;
