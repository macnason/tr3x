//! A T3 Code client for t3rex: pairing with each machine that runs T3, one connection per
//! machine, and T3's own thread and sidebar logic.
//!
//! The wire client, projection, models, launch and transcript code are forked from
//! [t3term](https://github.com/krishhgg/t3term) (MIT).

pub mod auth;
pub mod descriptor;
pub mod env;
pub mod error;
pub mod http;
pub mod hub;
pub mod inbox;
pub mod join;
pub mod launch;
pub mod models;
pub mod pair;
pub mod projection;
pub mod rpc;
pub mod store;
pub mod surface;
pub mod transcript;
