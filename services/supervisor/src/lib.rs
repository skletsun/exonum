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

//! Supervisor is an [Exonum][exonum] service capable of the following activities:
//! - Service artifact deployment;
//! - Service instances creation;
//! - Changing consensus configuration;
//! - Changing service instances configuration.
//!
//! More information on the artifact/service lifecycle can be found in the documentation for
//! the Exonum [runtime module][runtime-docs].
//!
//! Supervisor service has two different operating modes: a "simple" mode and a "decentralized" mode.
//! The difference between modes is in the decision making approach:
//! - Within the decentralized mode, to deploy a service or apply a new configuration,
//!  no less than (2/3)+1 validators should reach a consensus;
//! - Within the simple mode, any decision is executed after a single validator approval.
//!
//! The simple mode can be useful if one network administrator manages all the validator nodes
//! or for testing purposes (e.g., to test service configuration with `TestKit`).
//! For a network with a low node confidence, consider using the decentralized mode.
//!
//! # Interaction
//!
//! The intended way to interact with supervisor is the REST API. To be precise, requests should
//! be sent to the one of the following endpoints: `deploy-artifact`, `propose-config` or
//! `confirm-config`. Once received, supervisor will convert the request into appropriate
//! transaction, sign it with the validator keys and broadcast for the rest of the network.
//!
//! Key point here is that user **should not** send transactions to the supervisor by himself.
//!
//! An expected format of requests for those endpoints is a serialized Protobuf message.
//!
//! To deploy an artifact, one (within the "simple" mode) or majority (within the "decentralized" mode)
//! of the nodes should receive a [`DeployRequest`] message through API.
//!
//! To request a config change, one node should receive a [`ConfigPropose`] message through API.
//! For the "simple" mode no more actions are required. For the "decentralized" mode the majority of the nodes
//! should also receive [`ConfigVote`] messages with a hash of the proposed configuration.
//! The proposal initiator that receives the original [`ConfigPropose`] message must not vote for the configuration.
//! This node votes for the configuration propose automatically.
//!
//! The operation of starting a service is treated similarly to a configuration change and follows the same rules.
//!
//! [exonum]: https://github.com/exonum/exonum
//! [runtime-docs]: https://docs.rs/exonum/0.13.0/exonum/runtime/index.html
//! [`DeployRequest`]: struct.DeployRequest.html
//! [`ConfigPropose`]: struct.ConfigPropose.html
//! [`ConfigVote`]: struct.ConfigVote.html

#![deny(
    missing_debug_implementations,
    missing_docs,
    unsafe_code,
    bare_trait_objects
)]

pub use self::{
    configure::{Configure, ConfigureCall, CONFIGURE_INTERFACE_NAME},
    errors::Error,
    proto_structures::{
        ConfigChange, ConfigProposalWithHash, ConfigPropose, ConfigVote, DeployConfirmation,
        DeployRequest, ServiceConfig, StartService,
    },
    schema::Schema,
    transactions::SupervisorInterface,
};

use exonum::{
    crypto::Hash,
    runtime::{
        rust::{
            api::ServiceApiBuilder, AfterCommitContext, Broadcaster, BuiltinInstance, CallContext,
            Service,
        },
        BlockchainData, InstanceId, SUPERVISOR_INSTANCE_ID,
    },
};
use exonum_derive::*;
use exonum_merkledb::Snapshot;

pub mod mode;

mod api;
mod configure;
mod errors;
mod multisig;
mod proto;
mod proto_structures;
mod schema;
mod transactions;

/// Decentralized supervisor.
///
/// Within the "decentralized" mode, both deploy requests and configuration change proposals
/// should be approved by (2/3+1) validators.
pub type DecentralizedSupervisor = Supervisor<mode::Decentralized>;

/// Simple supervisor.
///
/// Within the "simple" mode, both deploy requests and configuration change proposals require
/// only one approval from a validator node.
pub type SimpleSupervisor = Supervisor<mode::Simple>;

/// Returns the `Supervisor` entity name.
pub const fn supervisor_name() -> &'static str {
    Supervisor::<mode::Decentralized>::NAME
}

/// Error message emitted when the `Supervisor` is installed as a non-privileged service.
const NOT_SUPERVISOR_MSG: &str = "`Supervisor` is installed as a non-privileged service. \
                                  For correct operation, `Supervisor` needs to have numeric ID 0.";

/// Applies configuration changes.
/// Upon any failure, execution of this method stops and `Err(())` is returned.
fn update_configs(context: &mut CallContext<'_>, changes: Vec<ConfigChange>) -> Result<(), ()> {
    for change in changes.into_iter() {
        match change {
            ConfigChange::Consensus(config) => {
                log::trace!("Updating consensus configuration {:?}", config);

                context
                    .writeable_core_schema()
                    .consensus_config_entry()
                    .set(config);
            }

            ConfigChange::Service(config) => {
                log::trace!(
                    "Updating service instance configuration, instance ID is {}",
                    config.instance_id
                );

                // `ConfigureCall` interface was checked during the config verifying
                // so panic on `expect` here is unlikely and means a bug in the implementation.
                context
                    .interface::<ConfigureCall<'_>>(config.instance_id)
                    .expect("Obtaining Configure interface failed")
                    .apply_config(config.params.clone())
                    .map_err(|e| {
                        log::error!(
                            "An error occurred while applying service configuration. {}",
                            e
                        );
                    })?;
            }

            ConfigChange::StartService(start_service) => {
                log::trace!(
                    "Request add service with name {:?} from artifact {:?}",
                    start_service.name,
                    start_service.artifact
                );

                let id = assign_instance_id(context);
                let (instance_spec, config) = start_service.into_parts(id);

                context
                    .start_adding_service(instance_spec, config)
                    .map_err(|e| {
                        log::error!("Service start request failed. {}", e);
                    })?;
            }
        }
    }
    Ok(())
}

/// Assigns the instance ID for a new service, initializing the schema `vacant_instance_id`
/// entry if needed.
fn assign_instance_id(context: &CallContext<'_>) -> InstanceId {
    let mut schema = Schema::new(context.service_data());
    match schema.assign_instance_id() {
        Some(id) => id,
        None => {
            // Instance ID entry is not initialized, do it now.
            // We have to do it lazy, since dispatcher doesn't know the amount
            // of builtin instances until the genesis block is committed, and
            // `before_commit` hook is not invoked for services at the genesis
            // block.

            // ID for the new instance is next to the highest builtin ID to avoid
            // overlap if builtin identifiers space is sparse.
            let dispatcher_schema = context.data().for_dispatcher();
            let builtin_instances = dispatcher_schema.running_instances();

            let new_instance_id = builtin_instances
                .values()
                .map(|spec| spec.id)
                .max()
                .unwrap_or(SUPERVISOR_INSTANCE_ID)
                + 1;

            // We're going to use ID obtained above, so the vacant ID is next to it.
            let vacant_instance_id = new_instance_id + 1;
            schema.vacant_instance_id.set(vacant_instance_id);

            new_instance_id
        }
    }
}

/// Supervisor service implementation.
#[derive(Debug, Default, Clone, ServiceFactory, ServiceDispatcher)]
#[service_dispatcher(implements("transactions::SupervisorInterface"))]
#[service_factory(
    proto_sources = "proto",
    artifact_name = "exonum-supervisor",
    service_constructor = "Self::construct"
)]
pub struct Supervisor<Mode>
where
    Mode: mode::SupervisorMode,
{
    phantom: std::marker::PhantomData<Mode>,
}

impl<Mode> Supervisor<Mode>
where
    Mode: mode::SupervisorMode,
{
    /// Name of the supervisor service.
    pub const NAME: &'static str = "supervisor";

    /// Creates a new `Supervisor` service object.
    pub fn new() -> Supervisor<Mode> {
        Supervisor {
            phantom: std::marker::PhantomData::<Mode>::default(),
        }
    }

    /// Factory constructor of the `Supervisor` object that takes `&self` as an argument.
    /// The constructor is required for the `ServiceFactory` trait implementation.
    pub fn construct(&self) -> Box<Self> {
        Box::new(Self::new())
    }
}

impl<Mode> Service for Supervisor<Mode>
where
    Mode: mode::SupervisorMode,
{
    fn state_hash(&self, data: BlockchainData<&dyn Snapshot>) -> Vec<Hash> {
        Schema::new(data.for_executing_service()).state_hash()
    }

    fn before_commit(&self, mut context: CallContext<'_>) {
        let mut schema = Schema::new(context.service_data());
        let core_schema = context.data().for_core();
        let validator_count = core_schema.consensus_config().validator_keys.len();
        let height = core_schema.height();

        // Removes pending deploy requests for which deadline was exceeded.
        let requests_to_remove = schema
            .pending_deployments
            .values()
            .filter(|request| request.deadline_height < height)
            .collect::<Vec<_>>();

        for request in requests_to_remove {
            schema.pending_deployments.remove(&request.artifact);
            log::trace!("Removed outdated deployment request {:?}", request);
        }

        let entry = schema.pending_proposal.get();
        if let Some(entry) = entry {
            if entry.config_propose.actual_from <= height {
                // Remove pending config proposal for which deadline was exceeded.
                log::trace!("Removed outdated config proposal");
                schema.pending_proposal.remove();
            } else if entry.config_propose.actual_from == height.next() {
                // Config should be applied at the next height.
                if Mode::config_approved(
                    &entry.propose_hash,
                    &schema.config_confirms,
                    validator_count,
                ) {
                    log::info!(
                        "New configuration has been accepted: {:?}",
                        entry.config_propose
                    );

                    // Remove config from proposals.
                    // If the config update will fail, this entry will be restored due to rollback.
                    // However, it won't be actual anymore and will be removed at the next height.
                    schema.pending_proposal.remove();
                    drop(schema);

                    // Perform the application of configs.
                    let update_result = update_configs(&mut context, entry.config_propose.changes);

                    if update_result.is_err() {
                        // Panic will cause changes to be rolled back.
                        // TODO: Return error instead of panic once the signature
                        // of `before_commit` will allow it. [ECR-3811]
                        panic!("Config update failed")
                    }
                }
            }
        }
    }

    /// Sends confirmation transaction for unconfirmed deployment requests.
    fn after_commit(&self, mut context: AfterCommitContext<'_>) {
        let service_key = context.service_key();

        let deployments: Vec<_> = {
            let schema = Schema::new(context.service_data());
            schema
                .pending_deployments
                .values()
                .filter(|request| {
                    let confirmation = DeployConfirmation::from(request.clone());
                    !schema
                        .deploy_confirmations
                        .confirmed_by(&confirmation, &service_key)
                })
                .collect()
        };

        for unconfirmed_request in deployments {
            let artifact = unconfirmed_request.artifact.clone();
            let spec = unconfirmed_request.spec.clone();
            let tx_sender = context.broadcaster().map(Broadcaster::into_owned);

            let mut extensions = context.supervisor_extensions().expect(NOT_SUPERVISOR_MSG);
            // We should deploy the artifact for all nodes, but send confirmations only
            // if the node is a validator.
            extensions.start_deploy(artifact, spec, move || {
                if let Some(tx_sender) = tx_sender {
                    log::trace!(
                        "Sending confirmation for deployment request {:?}",
                        unconfirmed_request
                    );
                    let confirmation = DeployConfirmation::from(unconfirmed_request);
                    if let Err(e) = tx_sender.send(confirmation) {
                        log::error!("Cannot send confirmation: {}", e);
                    }
                }
                Ok(())
            });
        }
    }

    fn wire_api(&self, builder: &mut ServiceApiBuilder) {
        api::wire(builder)
    }
}

impl<Mode: mode::SupervisorMode> BuiltinInstance for Supervisor<Mode> {
    const INSTANCE_ID: u32 = SUPERVISOR_INSTANCE_ID;
    const INSTANCE_NAME: &'static str = Self::NAME;
}
