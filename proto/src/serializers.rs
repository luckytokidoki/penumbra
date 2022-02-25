//! Serializers for adjusting the Serde implementations derived from the Rust
//! proto types.
//!
//! This approach is inspired by the tendermint-rs implementation, and some of
//! the serializers are adapted from that code.

pub mod hexstr;

pub mod base64str;

pub mod bech32str;
