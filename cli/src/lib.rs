// Copyright 2020 The Exonum Team
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

//! Helper crate for secure and convenient configuration of the Exonum nodes.
//!
//! `exonum-cli` supports multi-stage configuration process made with safety in mind. It involves
//! 4 steps (or stages) and allows to configure and run multiple blockchain nodes without
//! need in exchanging private keys between administrators.
//!
//! # How to Run the Network
//!
//! 1. Generate common (template) part of the nodes configuration using `generate-template` command.
//!   Generated `.toml` file must be spread among all the nodes and must be used in the following
//!   configuration step.
//! 2. Generate public and secret (private) parts of the node configuration using `generate-config`
//!   command. At this step, Exonum will generate master key from which consensus and service
//!   validator keys are derived. Master key is stored in the encrypted file. Consensus secret key
//!   is used for communications between the nodes, while service secret key is used
//!   mainly to sign transactions generated by the node. Both secret keys may be encrypted with a
//!   password. The public part of the node configuration must be spread among all nodes, while the
//!   secret part must be only accessible by the node administrator only.
//! 3. Generate final node configuration using `finalize` command. Exonum combines secret part of
//!   the node configuration with public configurations of every other node, producing a single
//!   configuration file with all the necessary node and network settings.
//! 4. Use `run` command and provide it with final node configuration file produced at the previous
//!   step. If the secret keys are protected with passwords, the user need to enter the password.
//!   Running node will automatically connect to other nodes in the network using IP addresses from
//!   public parts of the node configurations.
//!
//! ## Additional Commands
//!
//! `exonum-cli` also supports additional CLI commands for performing maintenance actions by node
//! administrators and easier debugging.
//!
//! * `run-dev` command automatically generates network configuration with a single node and runs
//! it. This command can be useful for fast testing of the services during development process.
//! * `maintenance` command allows to clear node's consensus messages with `clear-cache`, and
//! restart node's service migration script with `restart-migration`.
//!
//! ## How to Extend Parameters
//!
//! `exonum-cli` allows to extend the list of the parameters for any command and even add new CLI
//! commands with arbitrary behavior. To do so, you need to implement a structure with a list of
//! additional parameters and use `flatten` macro attribute of [`serde`][serde] and
//! [`structopt`][structopt] libraries.
//!
//! ```ignore
//! #[derive(Serialize, Deserialize, StructOpt)]
//! struct MyRunCommand {
//!     #[serde(flatten)]
//!     #[structopt(flatten)]
//!     default: Run
//!     /// My awesome parameter
//!     secret_number: i32
//! }
//! ```
//!
//! You can also create own list of commands by implementing an enum with a similar principle:
//!
//! ```ignore
//! #[derive(StructOpt)]
//! enum MyCommands {
//!     #[structopt(name = "run")
//!     DefaultRun(Run),
//!     #[structopt(name = "my-run")
//!     MyAwesomeRun(MyRunCommand),
//! }
//! ```
//!
//! While implementing custom behavior for your commands, you may use
//! [`StandardResult`](./command/enum.StandardResult.html) enum for
//! accessing node configuration files created and filled by the standard Exonum commands.
//!
//! [serde]: https://crates.io/crates/serde
//! [structopt]: https://crates.io/crates/structopt

#![deny(missing_docs)]

pub use crate::config_manager::DefaultConfigManager;
pub use structopt;

use exonum::{
    blockchain::config::{GenesisConfig, GenesisConfigBuilder, InstanceInitParams},
    merkledb::RocksDB,
    runtime::{RuntimeInstance, WellKnownRuntime},
};
use exonum_explorer_service::ExplorerFactory;
use exonum_node::{Node, NodeBuilder as CoreNodeBuilder};
use exonum_rust_runtime::{DefaultInstance, RustRuntimeBuilder, ServiceFactory};
use exonum_supervisor::{Supervisor, SupervisorConfig};
use exonum_system_api::SystemApiPlugin;
use structopt::StructOpt;
use tempfile::TempDir;

use std::{env, ffi::OsString, iter, path::PathBuf};

use crate::command::{run::NodeRunConfig, Command, ExonumCommand, StandardResult};

pub mod command;
pub mod config;
pub mod io;
pub mod password;

mod config_manager;

/// Rust-specific node builder used for constructing a node with a list
/// of provided services.
#[derive(Debug)]
pub struct NodeBuilder {
    rust_runtime: RustRuntimeBuilder,
    external_runtimes: Vec<RuntimeInstance>,
    builtin_instances: Vec<InstanceInitParams>,
    args: Option<Vec<OsString>>,
    temp_dir: Option<TempDir>,
}

impl Default for NodeBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl NodeBuilder {
    /// Creates a new builder.
    pub fn new() -> Self {
        Self {
            rust_runtime: RustRuntimeBuilder::new()
                .with_factory(Supervisor)
                .with_factory(ExplorerFactory),
            external_runtimes: vec![],
            builtin_instances: vec![],
            args: None,
            temp_dir: None,
        }
    }

    /// Creates a new builder with the provided command-line arguments. The path
    /// to the current executable **does not** need to be specified as the first argument.
    #[doc(hidden)] // unstable
    pub fn with_args<I>(args: I) -> Self
    where
        I: IntoIterator,
        I::Item: Into<OsString>,
    {
        let mut this = Self::new();
        let executable = env::current_exe()
            .map(PathBuf::into_os_string)
            .unwrap_or_else(|_| "node".into());
        let all_args = iter::once(executable)
            .chain(args.into_iter().map(Into::into))
            .collect();
        this.args = Some(all_args);
        this
    }

    /// Creates a single-node development network with default settings. The node stores
    /// its data in a temporary directory, which is automatically removed when the node is stopped.
    ///
    /// # Return value
    ///
    /// Returns an error if the temporary directory cannot be created.
    pub fn development_node() -> Result<Self, failure::Error> {
        let temp_dir = TempDir::new()?;
        let mut this = Self::with_args(vec![
            OsString::from("run-dev"),
            OsString::from("--artifacts-dir"),
            temp_dir.path().into(),
        ]);
        this.temp_dir = Some(temp_dir);
        Ok(this)
    }

    /// Adds new Rust service to the list of available services.
    pub fn with_rust_service(mut self, service: impl ServiceFactory) -> Self {
        self.rust_runtime = self.rust_runtime.with_factory(service);
        self
    }

    /// Adds a new `Runtime` to the list of available runtimes.
    ///
    /// Note that you don't have to add the Rust runtime, since it is included by default.
    pub fn with_external_runtime(mut self, runtime: impl WellKnownRuntime) -> Self {
        self.external_runtimes.push(runtime.into());
        self
    }

    /// Adds a service instance that will be available immediately after creating a genesis block.
    ///
    /// For Rust services, the service factory needs to be separately supplied
    /// via [`with_rust_service`](#method.with_rust_service).
    pub fn with_instance(mut self, instance: impl Into<InstanceInitParams>) -> Self {
        self.builtin_instances.push(instance.into());
        self
    }

    /// Adds a default Rust service instance that will be available immediately after creating a
    /// genesis block.
    pub fn with_default_rust_service(self, service: impl DefaultInstance) -> Self {
        self.with_instance(service.default_instance())
            .with_rust_service(service)
    }

    /// Executes a command received from the command line.
    ///
    /// # Return value
    ///
    /// Returns:
    ///
    /// - `Ok(Some(_))` if the command lead to the node creation
    /// - `Ok(None)` if the command executed successfully and did not lead to node creation
    /// - `Err(_)` if an error occurred during command execution
    #[doc(hidden)] // unstable
    pub fn execute_command(self) -> Result<Option<Node>, failure::Error> {
        let command = if let Some(args) = self.args {
            Command::from_iter(args)
        } else {
            Command::from_args()
        };

        if let StandardResult::Run(run_config) = command.execute()? {
            let genesis_config = Self::genesis_config(&run_config, self.builtin_instances);

            let db_options = &run_config.node_config.private_config.database;
            let database = RocksDB::open(run_config.db_path, db_options)?;

            let node_config_path = run_config.node_config_path.to_string_lossy();
            let config_manager = DefaultConfigManager::new(node_config_path.into_owned());
            let rust_runtime = self.rust_runtime;

            let node_config = run_config.node_config.into();
            let node_keys = run_config.node_keys;

            let mut node_builder = CoreNodeBuilder::new(database, node_config, node_keys)
                .with_genesis_config(genesis_config)
                .with_config_manager(config_manager)
                .with_plugin(SystemApiPlugin)
                .with_runtime_fn(|channel| rust_runtime.build(channel.endpoints_sender()));
            for runtime in self.external_runtimes {
                node_builder = node_builder.with_runtime(runtime);
            }
            Ok(Some(node_builder.build()))
        } else {
            Ok(None)
        }
    }

    /// Configures the node using parameters provided by user from stdin and then runs it.
    pub fn run(mut self) -> Result<(), failure::Error> {
        // Store temporary directory until the node is done.
        let _temp_dir = self.temp_dir.take();
        if let Some(node) = self.execute_command()? {
            node.run()
        } else {
            Ok(())
        }
    }

    fn genesis_config(
        run_config: &NodeRunConfig,
        default_instances: Vec<InstanceInitParams>,
    ) -> GenesisConfig {
        let mut builder = GenesisConfigBuilder::with_consensus_config(
            run_config.node_config.public_config.consensus.clone(),
        );
        // Add builtin services to genesis config.
        builder = builder
            .with_artifact(Supervisor.artifact_id())
            .with_instance(Self::supervisor_service(&run_config))
            .with_artifact(ExplorerFactory.artifact_id())
            .with_instance(ExplorerFactory.default_instance());
        // Add default instances.
        for instance in default_instances {
            builder = builder
                .with_artifact(instance.instance_spec.artifact.clone())
                .with_instance(instance)
        }
        builder.build()
    }

    fn supervisor_service(run_config: &NodeRunConfig) -> InstanceInitParams {
        let mode = run_config
            .node_config
            .public_config
            .general
            .supervisor_mode
            .clone();
        Supervisor::builtin_instance(SupervisorConfig { mode })
    }
}
