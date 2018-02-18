// Copyright 2017 The Exonum Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//   http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! This crate implements a *configuration service* for Exonum blockchain framework.
//!
//! Upon being plugged in, the service allows to modify Exonum blockchain configuration
//! using [proposals](struct.Propose.html) and [voting for proposal](struct.Vote.html),
//! both of which are implemented as transactions signed by blockchain validators.
//!
//! The service also provides HTTP API for public queries (get actual/following
//! configuration, etc.) and private queries, intended for use only by validator nodes' maintainers
//! (post configuration propose, post vote for a configuration propose).
//!
//! # Blockchain configuration
//!
//! Blockchain configuration corresponds to [`StoredConfiguration`][sc]
//! in the Exonum core library. The logic of the configuration service extensively uses
//! hashes of configuration, which are calculated as follows:
//!
//! 1. Parse a `StoredConfiguration` from JSON string if necessary.
//! 2. Convert a `StoredConfiguration` into bytes as per its `StorageValue` implementation.
//! 3. Use `exonum::crypto::hash()` on the obtained bytes.
//!
//! [sc]: https://docs.rs/exonum/0.3.0/exonum/blockchain/config/struct.StoredConfiguration.html
//!
//! # Examples
//!
//! ```rust,no_run
//! extern crate exonum;
//! extern crate exonum_configuration;
//!
//! use exonum::helpers::fabric::NodeBuilder;
//!
//! use exonum_configuration::ConfigurationServiceFactory;
//!
//! fn main() {
//!     exonum::helpers::init_logger().unwrap();
//!     NodeBuilder::new()
//!         .with_service(Box::new(ConfigurationServiceFactory))
//!         .run();
//! }
//! ```

#[macro_use]
extern crate exonum;
#[cfg(test)]
#[macro_use]
extern crate exonum_testkit;
#[cfg(test)]
#[macro_use]
extern crate pretty_assertions;
#[macro_use]
extern crate log;
#[macro_use]
extern crate serde_derive;
extern crate iron;
extern crate router;
extern crate bodyparser;
extern crate params;
#[macro_use]
extern crate lazy_static;

use router::Router;
use iron::Handler;
use exonum::api::Api;
use exonum::blockchain::{Service, Transaction, ApiContext};
use exonum::helpers::fabric::{ServiceFactory, Context};
use exonum::crypto::Hash;
use exonum::messages::RawTransaction;
use exonum::storage::Snapshot;
use exonum::encoding::{Error as EncodingError};

pub mod api;
pub mod schema;
pub mod transactions;

#[cfg(test)]
mod tests;

#[doc(no_inline)]
pub use schema::{ConfigurationSchema, ProposeData};
#[doc(no_inline)]
pub use transactions::{Propose, Vote};

/// Service identifier for the configuration service.
pub const CONFIGURATION_SERVICE_ID: u16 = 1;

/// Configuration service.
#[derive(Default)]
pub struct ConfigurationService {}

impl ConfigurationService {
    pub fn new() -> ConfigurationService {
        ConfigurationService {}
    }
}

impl Service for ConfigurationService {
    fn service_name(&self) -> &'static str {
        "configuration"
    }

    fn service_id(&self) -> u16 {
        CONFIGURATION_SERVICE_ID
    }

    fn state_hash(&self, snapshot: &Snapshot) -> Vec<Hash> {
        let schema = ConfigurationSchema::new(snapshot);
        schema.state_hash()
    }

    fn tx_from_raw(&self, raw: RawTransaction) -> Result<Box<Transaction>, EncodingError> {
        transactions::tx_from_raw(raw)
    }

    fn public_api_handler(&self, ctx: &ApiContext) -> Option<Box<Handler>> {
        let mut router = Router::new();
        let api = api::PublicConfigApi { blockchain: ctx.blockchain().clone() };
        api.wire(&mut router);
        Some(Box::new(router))
    }

    fn private_api_handler(&self, ctx: &ApiContext) -> Option<Box<Handler>> {
        let mut router = Router::new();
        let api = api::PrivateConfigApi {
            channel: ctx.node_channel().clone(),
            config: (*ctx.public_key(), ctx.secret_key().clone()),
        };
        api.wire(&mut router);
        Some(Box::new(router))
    }
}

/// A configuration service creator for the `NodeBuilder`.
#[derive(Debug)]
pub struct ConfigurationServiceFactory;

impl ServiceFactory for ConfigurationServiceFactory {
    fn make_service(&mut self, _: &Context) -> Box<Service> {
        Box::new(ConfigurationService::new())
    }
}
