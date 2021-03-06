// Copyright 2019 The Exonum Team
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

//! Module with available running modes for Supervisor.
//!
//! Currently two modes are available:
//! - Simple mode. Within simple mode, the amount of checks performed by the Supervisor
//!   is reduced to the minimum: the only requirement is that every request is sent
//!   by the validator.
//! - Decentralized mode. Within decentralized mode, deploy requests
//!   and config proposals should be approved by at least (2/3+1) validators.

use exonum::helpers::byzantine_quorum;
use exonum_crypto::Hash;
use exonum_merkledb::access::Access;

use super::{multisig::MultisigIndex, DeployRequest};

/// Simple supervisor mode: to deploy service one have to send
/// one request to any of the validators.
#[derive(Debug, Clone, Copy, Default)]
pub struct Simple;

/// Decentralized supervisor mode: to deploy service a request should be
/// sent to **every** validator before it will be executed.
/// For configs, a byzantine majority of validators should vote for it.
#[derive(Debug, Clone, Copy, Default)]
pub struct Decentralized;

/// Trait encapsulating the decision making logic of the supervisor.
pub trait SupervisorMode: std::fmt::Debug + Send + Sync + Copy + 'static {
    /// Checks whether deploy should be performed within the network.
    fn deploy_approved<T: Access>(
        deploy: &DeployRequest,
        deploy_requests: &MultisigIndex<T, DeployRequest>,
        validators: usize,
    ) -> bool;

    /// Checks whether config can be applied for the network.
    fn config_approved<T: Access>(
        config_hash: &Hash,
        config_confirms: &MultisigIndex<T, Hash>,
        validators: usize,
    ) -> bool;
}

impl SupervisorMode for Simple {
    fn deploy_approved<T: Access>(
        deploy: &DeployRequest,
        deploy_requests: &MultisigIndex<T, DeployRequest>,
        _validators: usize,
    ) -> bool {
        // For simple supervisor request from 1 validator is enough.
        deploy_requests.confirmations(deploy) >= 1
    }

    fn config_approved<T: Access>(
        config_hash: &Hash,
        config_confirms: &MultisigIndex<T, Hash>,
        _validators: usize,
    ) -> bool {
        config_confirms.confirmations(&config_hash) >= 1
    }
}

impl SupervisorMode for Decentralized {
    fn deploy_approved<T: Access>(
        deploy: &DeployRequest,
        deploy_requests: &MultisigIndex<T, DeployRequest>,
        validators: usize,
    ) -> bool {
        let confirmations = deploy_requests.confirmations(&deploy);

        // Approve deploy in case 2/3+1 validators confirmed it.
        confirmations >= byzantine_quorum(validators)
    }

    fn config_approved<T: Access>(
        config_hash: &Hash,
        config_confirms: &MultisigIndex<T, Hash>,
        validators: usize,
    ) -> bool {
        let confirmations = config_confirms.confirmations(&config_hash);

        // Apply pending config in case 2/3+1 validators voted for it.
        confirmations >= byzantine_quorum(validators)
    }
}
