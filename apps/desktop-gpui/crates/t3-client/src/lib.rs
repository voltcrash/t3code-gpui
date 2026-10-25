//! Client side of the T3 Code server protocol, reimplemented in Rust for the
//! GPUI desktop app. The server stays the source of truth; nothing here may
//! require a server change.

pub mod archives;
pub mod auth;
pub mod bootstrap_token;
pub mod commands;
pub mod config;
pub mod projects;
pub mod providers;
pub mod requests;
pub mod rpc;
pub mod shell;
pub mod thread;
pub mod thread_actions;

pub use rpc::{RpcClient, RpcError, Subscription};
