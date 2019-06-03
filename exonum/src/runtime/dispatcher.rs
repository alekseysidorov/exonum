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

use exonum_merkledb::{Fork, Snapshot};
use futures::{future::Future, sink::Sink, sync::mpsc};

use std::collections::HashMap;

use crate::{
    api::ServiceApiBuilder,
    events::InternalRequest,
    {crypto::Hash, messages::CallInfo},
};

use super::{
    error::{DeployError, ExecutionError, InitError, WRONG_RUNTIME},
    rust::{service::ServiceFactory, RustRuntime},
    ArtifactSpec, DeployStatus, Runtime, RuntimeContext, ServiceConstructor, ServiceInstanceId,
};

pub struct Dispatcher {
    runtimes: HashMap<u32, Box<dyn Runtime>>,
    // TODO Is RefCell enough here?
    runtime_lookup: HashMap<ServiceInstanceId, u32>,
    inner_requests_tx: mpsc::Sender<InternalRequest>,
}

impl std::fmt::Debug for Dispatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "Dispatcher entity")
    }
}

impl Dispatcher {
    pub fn new(inner_requests_tx: mpsc::Sender<InternalRequest>) -> Self {
        Self::with_runtimes(Default::default(), inner_requests_tx)
    }

    pub fn with_runtimes(
        runtimes: HashMap<u32, Box<dyn Runtime>>,
        inner_requests_tx: mpsc::Sender<InternalRequest>,
    ) -> Self {
        Self {
            runtimes,
            runtime_lookup: Default::default(),
            inner_requests_tx,
        }
    }

    pub fn add_runtime(&mut self, id: u32, runtime: impl Into<Box<dyn Runtime>>) {
        self.runtimes.insert(id, runtime.into());
    }

    pub(crate) fn notify_service_started(
        &mut self,
        service_id: ServiceInstanceId,
        artifact: ArtifactSpec,
    ) {
        self.runtime_lookup.insert(service_id, artifact.runtime_id);
    }

    // TODO think about runtime environment traits. [ECR-3222]

    pub fn start_deploy(&mut self, artifact: ArtifactSpec) -> Result<(), DeployError> {
        if let Some(runtime) = self.runtimes.get_mut(&artifact.runtime_id) {
            runtime.start_deploy(artifact)
        } else {
            Err(DeployError::WrongRuntime)
        }
    }

    pub fn check_deploy_status(
        &self,
        artifact: ArtifactSpec,
        cancel_if_not_complete: bool,
    ) -> Result<DeployStatus, DeployError> {
        if let Some(runtime) = self.runtimes.get(&artifact.runtime_id) {
            runtime.check_deploy_status(artifact, cancel_if_not_complete)
        } else {
            Err(DeployError::WrongRuntime)
        }
    }

    pub fn init_service(
        &mut self,
        ctx: &mut RuntimeContext,
        artifact: ArtifactSpec,
        constructor: &ServiceConstructor,
    ) -> Result<(), InitError> {
        if let Some(runtime) = self.runtimes.get_mut(&artifact.runtime_id) {
            let result = runtime.init_service(ctx, artifact.clone(), &constructor);
            if result.is_ok() {
                self.notify_service_started(constructor.instance_id, artifact);
            }

            let _ = self
                .inner_requests_tx
                .clone()
                .send(InternalRequest::RestartApi)
                .wait()
                .map_err(|e| error!("Failed to request API restart: {}", e));

            result
        } else {
            Err(InitError::WrongRuntime)
        }
    }

    pub fn execute(
        &mut self,
        context: &mut RuntimeContext,
        call_info: CallInfo,
        payload: &[u8],
    ) -> Result<(), ExecutionError> {
        let runtime_id = self.runtime_lookup.get(&call_info.instance_id);

        if runtime_id.is_none() {
            return Err(ExecutionError::with_description(
                WRONG_RUNTIME,
                "Wrong runtime",
            ));
        }

        if let Some(runtime) = self.runtimes.get(&runtime_id.unwrap()) {
            runtime.execute(context, call_info, payload)?;
            // Executes pending dispatcher actions.
            context
                .take_dispatcher_actions()
                .into_iter()
                .try_for_each(|action| action.execute(self, context))
        } else {
            Err(ExecutionError::with_description(
                WRONG_RUNTIME,
                "Wrong runtime",
            ))
        }
    }

    pub fn state_hashes(&self, snapshot: &dyn Snapshot) -> Vec<(ServiceInstanceId, Vec<Hash>)> {
        self.runtimes
            .iter()
            .map(|(_, runtime)| runtime.state_hashes(snapshot))
            .flatten()
            .collect::<Vec<_>>()
    }

    pub fn before_commit(&self, fork: &mut Fork) {
        for (_, runtime) in &self.runtimes {
            runtime.before_commit(fork);
        }
    }

    pub fn after_commit(&self, fork: &Fork) {
        for (_, runtime) in &self.runtimes {
            runtime.after_commit(fork);
        }
    }

    pub fn services_api(&self) -> Vec<(String, ServiceApiBuilder)> {
        self.runtimes
            .iter()
            .fold(Vec::new(), |mut api, (_, runtime)| {
                api.append(&mut runtime.services_api());
                api
            })
    }
}

#[derive(Debug)]
pub struct DispatcherBuilder {
    builtin_runtime: RustRuntime,
    dispatcher: Dispatcher,
}

#[derive(Debug)]
pub struct BuiltinService {
    pub factory: Box<dyn ServiceFactory>,
    pub instance_id: ServiceInstanceId,
    pub instance_name: String,
}

impl DispatcherBuilder {
    pub fn new(requests: mpsc::Sender<InternalRequest>) -> Self {
        Self {
            dispatcher: Dispatcher::new(requests),
            builtin_runtime: RustRuntime::default(),
        }
    }

    /// Adds built-in service with predefined identifier, keep in mind that the initialize method
    /// of service will not be invoked and thus service must have and empty constructor.
    pub fn with_builtin_service(mut self, service: impl Into<BuiltinService>) -> Self {
        let service = service.into();
        // Registers service instance in runtime.
        let artifact = self.builtin_runtime.add_builtin_service(
            service.factory,
            service.instance_id,
            service.instance_name,
        );
        // Registers service instance in dispatcher.
        self.dispatcher
            .notify_service_started(service.instance_id, artifact);
        self
    }

    /// Adds service factory to the Rust runtime.
    pub fn with_service_factory(
        mut self,
        service_factory: impl Into<Box<dyn ServiceFactory>>,
    ) -> Self {
        self.builtin_runtime
            .add_service_factory(service_factory.into());
        self
    }

    /// Adds given service factories to the Rust runtime.
    pub fn with_service_factories(
        mut self,
        service_factories: impl IntoIterator<Item = impl Into<Box<dyn ServiceFactory>>>,
    ) -> Self {
        for factory in service_factories {
            self.builtin_runtime.add_service_factory(factory.into());
        }
        self
    }

    /// Adds additional runtime.
    pub fn with_runtime(mut self, id: u32, runtime: impl Into<Box<dyn Runtime>>) -> Self {
        self.dispatcher.add_runtime(id, runtime);
        self
    }

    pub fn finalize(mut self) -> Dispatcher {
        self.dispatcher
            .add_runtime(RustRuntime::ID as u32, self.builtin_runtime);
        self.dispatcher
    }
}

// TODO Update action names in according with changes in runtime. [ECR-3222]
#[derive(Debug)]
pub enum Action {
    StartDeploy {
        artifact: ArtifactSpec,
    },
    InitService {
        artifact: ArtifactSpec,
        constructor: ServiceConstructor,
    },
}

impl Action {
    fn execute(
        self,
        dispatcher: &mut Dispatcher,
        context: &mut RuntimeContext,
    ) -> Result<(), ExecutionError> {
        match self {
            Action::StartDeploy { artifact } => {
                dispatcher.start_deploy(artifact).map_err(From::from)
            }
            Action::InitService {
                artifact,
                constructor,
            } => dispatcher
                .init_service(context, artifact, &constructor)
                .map_err(From::from),
        }
    }
}

#[cfg(test)]
mod tests {
    use exonum_merkledb::{Database, TemporaryDB};

    use crate::{
        crypto::{Hash, PublicKey},
        messages::{MethodId, ServiceInstanceId},
        runtime::RuntimeIdentifier,
    };

    use super::*;

    impl DispatcherBuilder {
        fn dummy() -> Self {
            Self::new(mpsc::channel(0).0)
        }
    }

    #[derive(Debug)]
    struct SampleRuntime {
        pub runtime_type: u32,
        pub instance_id: ServiceInstanceId,
        pub method_id: MethodId,
    }

    impl SampleRuntime {
        pub fn new(runtime_type: u32, instance_id: ServiceInstanceId, method_id: MethodId) -> Self {
            Self {
                runtime_type,
                instance_id,
                method_id,
            }
        }
    }

    impl Runtime for SampleRuntime {
        fn start_deploy(&mut self, artifact: ArtifactSpec) -> Result<(), DeployError> {
            if artifact.runtime_id == self.runtime_type {
                Ok(())
            } else {
                Err(DeployError::WrongRuntime)
            }
        }

        fn check_deploy_status(
            &self,
            artifact: ArtifactSpec,
            _: bool,
        ) -> Result<DeployStatus, DeployError> {
            if artifact.runtime_id == self.runtime_type {
                Ok(DeployStatus::Deployed)
            } else {
                Err(DeployError::WrongRuntime)
            }
        }

        fn init_service(
            &mut self,
            _: &mut RuntimeContext,
            artifact: ArtifactSpec,
            _: &ServiceConstructor,
        ) -> Result<(), InitError> {
            if artifact.runtime_id == self.runtime_type {
                Ok(())
            } else {
                Err(InitError::WrongRuntime)
            }
        }

        fn execute(
            &self,
            _: &mut RuntimeContext,
            call_info: CallInfo,
            _: &[u8],
        ) -> Result<(), ExecutionError> {
            if call_info.instance_id == self.instance_id && call_info.method_id == self.method_id {
                Ok(())
            } else {
                Err(ExecutionError::new(0xFF_u8))
            }
        }

        fn state_hashes(&self, _snapshot: &dyn Snapshot) -> Vec<(ServiceInstanceId, Vec<Hash>)> {
            vec![]
        }

        fn before_commit(&self, _: &mut Fork) {}

        fn after_commit(&self, _: &Fork) {}
    }

    #[test]
    fn test_builder() {
        let runtime_a = SampleRuntime::new(RuntimeIdentifier::Rust as u32, 0, 0);
        let runtime_b = SampleRuntime::new(RuntimeIdentifier::Java as u32, 1, 0);

        let dispatcher = DispatcherBuilder::dummy()
            .with_runtime(runtime_a.runtime_type, runtime_a)
            .with_runtime(runtime_b.runtime_type, runtime_b)
            .finalize();

        assert!(dispatcher
            .runtimes
            .get(&(RuntimeIdentifier::Rust as u32))
            .is_some());
        assert!(dispatcher
            .runtimes
            .get(&(RuntimeIdentifier::Java as u32))
            .is_some());
    }

    #[test]
    fn test_dispatcher() {
        const RUST_SERVICE_ID: ServiceInstanceId = 0;
        const JAVA_SERVICE_ID: ServiceInstanceId = 1;
        const RUST_METHOD_ID: MethodId = 0;
        const JAVA_METHOD_ID: MethodId = 1;

        // Create dispatcher and test data.
        let db = TemporaryDB::new();

        let runtime_a = SampleRuntime::new(
            RuntimeIdentifier::Rust as u32,
            RUST_SERVICE_ID,
            RUST_METHOD_ID,
        );
        let runtime_b = SampleRuntime::new(
            RuntimeIdentifier::Java as u32,
            JAVA_SERVICE_ID,
            JAVA_METHOD_ID,
        );

        let mut dispatcher = DispatcherBuilder::dummy()
            .with_runtime(runtime_a.runtime_type, runtime_a)
            .with_runtime(runtime_b.runtime_type, runtime_b)
            .finalize();

        let sample_rust_spec = ArtifactSpec {
            runtime_id: RuntimeIdentifier::Rust as u32,
            raw_spec: Default::default(),
        };
        let sample_java_spec = ArtifactSpec {
            runtime_id: RuntimeIdentifier::Java as u32,
            raw_spec: Default::default(),
        };

        // Check deploy.
        dispatcher
            .start_deploy(sample_rust_spec.clone())
            .expect("start_deploy failed for rust");
        dispatcher
            .start_deploy(sample_java_spec.clone())
            .expect("start_deploy failed for java");

        // Check deploy status
        assert_eq!(
            dispatcher
                .check_deploy_status(sample_rust_spec.clone(), false)
                .unwrap(),
            DeployStatus::Deployed
        );
        assert_eq!(
            dispatcher
                .check_deploy_status(sample_java_spec.clone(), false)
                .unwrap(),
            DeployStatus::Deployed
        );

        // Check if we can init services.
        let mut fork = db.fork();
        let mut context = RuntimeContext::new(&mut fork, PublicKey::zero(), Hash::zero());

        let rust_init_data = ServiceConstructor {
            instance_id: RUST_SERVICE_ID,
            data: Default::default(),
        };
        dispatcher
            .init_service(&mut context, sample_rust_spec.clone(), &rust_init_data)
            .expect("init_service failed for rust");

        let java_init_data = ServiceConstructor {
            instance_id: JAVA_SERVICE_ID,
            data: Default::default(),
        };
        dispatcher
            .init_service(&mut context, sample_java_spec.clone(), &java_init_data)
            .expect("init_service failed for java");

        // Check if we can execute transactions.
        let tx_payload = [0x00_u8; 1];

        dispatcher
            .execute(
                &mut context,
                CallInfo::new(RUST_SERVICE_ID, RUST_METHOD_ID),
                &tx_payload,
            )
            .expect("Correct tx rust");

        dispatcher
            .execute(
                &mut context,
                CallInfo::new(RUST_SERVICE_ID, JAVA_METHOD_ID),
                &tx_payload,
            )
            .expect_err("Incorrect tx rust");

        dispatcher
            .execute(
                &mut context,
                CallInfo::new(JAVA_SERVICE_ID, JAVA_METHOD_ID),
                &tx_payload,
            )
            .expect("Correct tx java");

        dispatcher
            .execute(
                &mut context,
                CallInfo::new(JAVA_SERVICE_ID, RUST_METHOD_ID),
                &tx_payload,
            )
            .expect_err("Incorrect tx java");
    }

    #[test]
    fn test_dispatcher_no_service() {
        const RUST_SERVICE_ID: ServiceInstanceId = 0;
        const RUST_METHOD_ID: MethodId = 0;

        // Create dispatcher and test data.
        let db = TemporaryDB::new();

        let mut dispatcher = DispatcherBuilder::dummy().finalize();

        let sample_rust_spec = ArtifactSpec {
            runtime_id: RuntimeIdentifier::Rust as u32,
            raw_spec: Default::default(),
        };

        // Check deploy.
        assert_eq!(
            dispatcher
                .start_deploy(sample_rust_spec.clone())
                .expect_err("start_deploy succeed"),
            DeployError::WrongRuntime
        );

        assert_eq!(
            dispatcher
                .check_deploy_status(sample_rust_spec.clone(), false)
                .expect_err("check_deploy_status succeed"),
            DeployError::WrongRuntime
        );

        // Check if we can init services.
        let mut fork = db.fork();
        let mut context = RuntimeContext::new(&mut fork, PublicKey::zero(), Hash::zero());

        let rust_init_data = ServiceConstructor {
            instance_id: RUST_SERVICE_ID,
            data: Default::default(),
        };
        assert_eq!(
            dispatcher
                .init_service(&mut context, sample_rust_spec.clone(), &rust_init_data)
                .expect_err("init_service succeed"),
            InitError::WrongRuntime
        );

        // Check if we can execute transactions.
        let tx_payload = [0x00_u8; 1];

        dispatcher
            .execute(
                &mut context,
                CallInfo::new(RUST_SERVICE_ID, RUST_METHOD_ID),
                &tx_payload,
            )
            .expect_err("execute succeed");
    }
}
