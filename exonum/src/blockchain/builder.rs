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

//! The module responsible for the correct Exonum blockchain creation.

use futures::sync::mpsc;

use crate::{
    api::manager::UpdateEndpoints,
    blockchain::{Blockchain, BlockchainMut, ConsensusConfig, Schema},
    merkledb::BinaryValue,
    runtime::{
        rust::{RustRuntime, ServiceFactory},
        Dispatcher, InstanceId, InstanceSpec, Runtime,
    },
};

/// The object responsible for the correct Exonum blockchain creation from the components.
///
/// During the `Blockchain` creation it creates and commits a genesis block if the database
/// is empty. Otherwise, it restores the state from the database.
// TODO: refine interface [ECR-3744]
#[derive(Debug)]
pub struct BlockchainBuilder {
    /// Underlying shared blockchain instance.
    pub blockchain: Blockchain,
    /// List of the supported runtimes.
    pub runtimes: Vec<(u32, Box<dyn Runtime>)>,
    /// Blockchain configuration used to create the genesis block.
    pub genesis_config: ConsensusConfig,
    /// List of the privileged services with the configuration parameters that are created directly
    /// in the genesis block.
    pub builtin_instances: Vec<InstanceConfig>,
}

impl BlockchainBuilder {
    /// Creates a new builder instance based on the `Blockchain`.
    pub fn new(blockchain: Blockchain, genesis_config: ConsensusConfig) -> Self {
        Self {
            blockchain,
            genesis_config,
            runtimes: vec![],
            builtin_instances: vec![],
        }
    }

    /// Adds the built-in Rust runtime with the specified built-in services.
    pub fn with_rust_runtime(
        mut self,
        api_notifier: mpsc::Sender<UpdateEndpoints>,
        services: impl IntoIterator<Item = InstanceCollection>,
    ) -> Self {
        let mut runtime = RustRuntime::new(api_notifier);
        for service in services {
            runtime.add_service_factory(service.factory);
            self.builtin_instances.extend(service.instances);
        }
        self.with_additional_runtime(runtime)
    }

    /// Adds multiple runtimes with the specified identifiers and returns
    /// a modified `Self` object for further chaining.
    pub fn with_external_runtimes(
        mut self,
        runtimes: impl IntoIterator<Item = impl Into<(u32, Box<dyn Runtime>)>>,
    ) -> Self {
        for runtime in runtimes {
            self.runtimes.push(runtime.into());
        }
        self
    }

    /// Adds an additional runtime with the specified identifier and returns
    /// a modified `Self` object for further chaining.
    pub fn with_additional_runtime(mut self, runtime: impl Into<(u32, Box<dyn Runtime>)>) -> Self {
        self.runtimes.push(runtime.into());
        self
    }

    /// Adds instance specifications of the built-in services.
    pub fn with_builtin_instances(
        mut self,
        instances: impl IntoIterator<Item = InstanceConfig>,
    ) -> Self {
        self.builtin_instances.extend(instances);
        self
    }

    /// Returns blockchain instance, creates and commits the genesis block with the specified
    /// genesis configuration if the blockchain has not been initialized.
    /// Otherwise restores dispatcher state from database.
    ///
    /// # Panics
    ///
    /// * If the genesis block was not committed.
    /// * If storage version is not specified or not supported.
    pub fn build(self) -> Result<BlockchainMut, failure::Error> {
        let mut blockchain = BlockchainMut {
            dispatcher: Dispatcher::new(&self.blockchain, self.runtimes),
            inner: self.blockchain,
        };

        // If genesis block had been already created just restores dispatcher state from database
        // otherwise creates genesis block with the given specification.
        let snapshot = blockchain.snapshot();
        let has_genesis_block = !Schema::new(&snapshot).block_hashes_by_height().is_empty();

        if has_genesis_block {
            blockchain.dispatcher.restore_state(&snapshot)?;
        } else {
            // Adds builtin services.
            blockchain.create_genesis_block(self.genesis_config, self.builtin_instances)?;
        };
        Ok(blockchain)
    }
}

/// Instantiation parameters of service instance.
#[derive(Debug)]
pub struct InstanceConfig {
    /// Service instance specification.
    pub instance_spec: InstanceSpec,
    /// Artifact deploy specification.
    pub artifact_spec: Option<Vec<u8>>,
    /// Service configuration parameters.
    pub constructor: Vec<u8>,
}

impl InstanceConfig {
    /// Creates new instantiation parameters of the service instance.
    pub fn new(
        instance_spec: InstanceSpec,
        artifact_spec: Option<Vec<u8>>,
        constructor: Vec<u8>,
    ) -> Self {
        Self {
            instance_spec,
            artifact_spec,
            constructor,
        }
    }
}

/// Rust runtime artifact with the list of instances.
#[derive(Debug)]
pub struct InstanceCollection {
    /// Rust services factory as a special case of an artifact.
    pub factory: Box<dyn ServiceFactory>,
    /// List of service instances with the initial configuration parameters.
    pub instances: Vec<InstanceConfig>,
}

impl InstanceCollection {
    /// Creates a new blank collection of instances for the specified service factory.
    pub fn new(factory: impl Into<Box<dyn ServiceFactory>>) -> Self {
        Self {
            factory: factory.into(),
            instances: Vec::new(),
        }
    }

    /// Add a new service instance to the collection.
    pub fn with_instance(
        mut self,
        id: InstanceId,
        name: impl Into<String>,
        params: impl BinaryValue,
    ) -> Self {
        let spec = InstanceSpec {
            artifact: self.factory.artifact_id().into(),
            id,
            name: name.into(),
        };
        let instance_config = InstanceConfig::new(spec, None, params.into_bytes());
        self.instances.push(instance_config);
        self
    }
}

#[cfg(test)]
mod tests {
    use futures::sync::mpsc;

    use super::*;
    use crate::{
        // Import service from tests, so we won't have implement other one.
        blockchain::tests::ServiceGoodImpl as SampleService,
        helpers::{generate_testnet_config, Height},
    };

    #[test]
    fn finalize_without_genesis_block() {
        let config = generate_testnet_config(1, 0)[0].clone();
        let blockchain = Blockchain::build_for_tests()
            .into_mut(config.consensus)
            .with_rust_runtime(mpsc::channel(0).0, vec![])
            .build()
            .unwrap();

        let access = blockchain.snapshot();
        assert_eq!(Schema::new(access.as_ref()).height(), Height(0));
        // TODO check dispatcher schema.
    }

    fn test_finalizing_services(services: Vec<InstanceCollection>) {
        let config = generate_testnet_config(1, 0)[0].clone();
        Blockchain::build_for_tests()
            .into_mut(config.consensus)
            .with_rust_runtime(mpsc::channel(0).0, services)
            .build()
            .unwrap();
    }

    #[test]
    #[should_panic(expected = "already used")]
    fn finalize_duplicate_services() {
        test_finalizing_services(vec![
            InstanceCollection::new(SampleService).with_instance(0, "sample", ()),
            InstanceCollection::new(SampleService).with_instance(0, "sample", ()),
        ]);
    }

    #[test]
    #[should_panic(expected = "already used")]
    fn finalize_services_with_duplicate_names() {
        test_finalizing_services(vec![
            InstanceCollection::new(SampleService).with_instance(0, "sample", ()),
            InstanceCollection::new(SampleService).with_instance(1, "sample", ()),
        ]);
    }

    #[test]
    #[should_panic(expected = "already used")]
    fn finalize_services_with_duplicate_ids() {
        test_finalizing_services(vec![
            InstanceCollection::new(SampleService).with_instance(0, "sample", ()),
            InstanceCollection::new(SampleService).with_instance(0, "other-sample", ()),
        ]);
    }

    #[test]
    #[should_panic(expected = "Consensus configuration must have at least one validator")]
    fn finalize_invalid_consensus_config() {
        let consensus_config = ConsensusConfig::default();
        Blockchain::build_for_tests()
            .into_mut(consensus_config)
            .with_rust_runtime(mpsc::channel(0).0, vec![])
            .build()
            .unwrap();
    }
}
