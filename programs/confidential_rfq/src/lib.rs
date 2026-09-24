//! An owner-controlled confidential total bounded by five.
//!
//! The client wraps initialization and addition in Zama's top-level transient-store open/close.
//! An over-limit addition succeeds on-chain but selects the previous encrypted value.
//! Finalization verifies the latest acceptance flag in a later, read-only transaction.

#![allow(unexpected_cfgs)]

pub mod errors;
pub mod instructions;
pub mod state;
pub mod util;

pub use errors::*;
pub use instructions::*;
pub use state::*;

use anchor_lang::prelude::*;

declare_id!("7Y7wXXw2GWp6v3FMSQYrMqbB6re5cs2E8i7HqgKwCXpp");

#[program]
pub mod confidential_rfq {
    use super::*;
}
