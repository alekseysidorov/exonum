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

use exonum::{blockchain::ConsensusConfig, crypto::Hash};
use exonum_rust_runtime::{
    api::{self, ServiceApiBuilder, ServiceApiState},
    Broadcaster,
};
use failure::Fail;

use super::{
    schema::SchemaImpl, transactions::SupervisorInterface, ConfigProposalWithHash, ConfigPropose,
    ConfigVote, DeployRequest, SupervisorConfig,
};

/// Private API specification of the supervisor service.
pub trait PrivateApi {
    /// Error type for the current API implementation.
    type Error: Fail;

    /// Creates and broadcasts the `DeployArtifact` transaction, which is signed
    /// by the current node, and returns its hash.
    fn deploy_artifact(&self, artifact: DeployRequest) -> Result<Hash, Self::Error>;

    /// Creates and broadcasts the `ConfigPropose` transaction, which is signed
    /// by the current node, and returns its hash.
    fn propose_config(&self, proposal: ConfigPropose) -> Result<Hash, Self::Error>;

    /// Creates and broadcasts the `ConfigVote` transaction, which is signed
    /// by the current node, and returns its hash.
    fn confirm_config(&self, vote: ConfigVote) -> Result<Hash, Self::Error>;

    /// Returns the number of processed configurations.
    fn configuration_number(&self) -> Result<u64, Self::Error>;

    /// Returns an actual supervisor config.
    fn supervisor_config(&self) -> Result<SupervisorConfig, Self::Error>;
}

pub trait PublicApi {
    /// Error type for the current API implementation.
    type Error: Fail;
    /// Returns an actual consensus configuration of the blockchain.
    fn consensus_config(&self) -> Result<ConsensusConfig, Self::Error>;
    /// Returns an pending propose config change.
    fn config_proposal(&self) -> Result<Option<ConfigProposalWithHash>, Self::Error>;
}

struct ApiImpl<'a>(&'a ServiceApiState<'a>);

impl ApiImpl<'_> {
    fn broadcaster(&self) -> Result<Broadcaster<'_>, api::Error> {
        self.0
            .broadcaster()
            .ok_or_else(|| api::Error::BadRequest("Node is not a validator".to_owned()))
    }
}

impl PrivateApi for ApiImpl<'_> {
    type Error = api::Error;

    fn deploy_artifact(&self, artifact: DeployRequest) -> Result<Hash, Self::Error> {
        self.broadcaster()?
            .request_artifact_deploy((), artifact)
            .map_err(From::from)
    }

    fn propose_config(&self, proposal: ConfigPropose) -> Result<Hash, Self::Error> {
        self.broadcaster()?
            .propose_config_change((), proposal)
            .map_err(From::from)
    }

    fn confirm_config(&self, vote: ConfigVote) -> Result<Hash, Self::Error> {
        self.broadcaster()?
            .confirm_config_change((), vote)
            .map_err(From::from)
    }

    fn configuration_number(&self) -> Result<u64, Self::Error> {
        let configuration_number =
            SchemaImpl::new(self.0.service_data()).get_configuration_number();
        Ok(configuration_number)
    }

    fn supervisor_config(&self) -> Result<SupervisorConfig, Self::Error> {
        let config = SchemaImpl::new(self.0.service_data()).supervisor_config();
        Ok(config)
    }
}

impl PublicApi for ApiImpl<'_> {
    type Error = api::Error;

    fn consensus_config(&self) -> Result<ConsensusConfig, Self::Error> {
        Ok(self.0.data().for_core().consensus_config())
    }

    fn config_proposal(&self) -> Result<Option<ConfigProposalWithHash>, Self::Error> {
        Ok(SchemaImpl::new(self.0.service_data())
            .public
            .pending_proposal
            .get())
    }
}

pub fn wire(builder: &mut ServiceApiBuilder) {
    builder
        .private_scope()
        .endpoint_mut("deploy-artifact", |state, query| {
            ApiImpl(state).deploy_artifact(query)
        })
        .endpoint_mut("propose-config", |state, query| {
            ApiImpl(state).propose_config(query)
        })
        .endpoint_mut("confirm-config", |state, query| {
            ApiImpl(state).confirm_config(query)
        })
        .endpoint("configuration-number", |state, _query: ()| {
            ApiImpl(state).configuration_number()
        })
        .endpoint("supervisor-config", |state, _query: ()| {
            ApiImpl(state).supervisor_config()
        });
    builder
        .public_scope()
        .endpoint("consensus-config", |state, _query: ()| {
            ApiImpl(state).consensus_config()
        })
        .endpoint("config-proposal", |state, _query: ()| {
            ApiImpl(state).config_proposal()
        });
}
