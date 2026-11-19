//! ProjectNeb Pro client: entitlement tokens, the encrypted licence store and the account flow.
//!
//! The worker issues a 15-minute Ed25519 entitlement and a 30-day session token. This module is the
//! only place in the workspace that parses them:
//!
//! * [`jwt`] verifies EdDSA only, rejects `alg: none` and unknown key ids, and hands the claims back;
//! * [`jwks`] reads `/api/keys/jwks` and keeps its 24-hour cache under the data directory;
//! * [`store`] keeps the tokens in DPAPI-encrypted `license.json` and decides how long an expired
//!   plan may still be believed offline;
//! * [`account`] performs the HTTP calls and renders the `account.getState` / `license.getState`
//!   documents;
//! * [`loopback`] is the `127.0.0.1` listener the Discord callback returns to;
//! * [`http`] is a small client: plain sockets for the local stub, WinHTTP for the worker.
//!
//! No part of the module runs a background thread or an async runtime.

pub mod account;
pub mod dpapi;
pub mod ed25519;
pub mod http;
pub mod ids;
pub mod jwks;
pub mod jwt;
pub mod loopback;
pub mod store;
#[cfg(feature = "test-support")]
pub mod testing;
