use crate::envoy::kuadrant::v1::{
    GetServiceDescriptorsRequest, GetServiceDescriptorsResponse, ServiceRef,
};
use prost::Message;
use prost_reflect::DescriptorPool;
use prost_types::FileDescriptorSet;
use proxy_wasm::traits::Context;
use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::Hash;
use std::rc::Rc;
use std::time::Duration;
use tracing::{debug, error};

pub const DESCRIPTOR_FETCH_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct DescriptorKey {
    pub cluster: String,
    pub service: String,
}

impl DescriptorKey {
    pub fn new(cluster: String, service: String) -> Self {
        Self { cluster, service }
    }
}

#[derive(Debug)]
pub enum DescriptorError {
    NotAvailable { cluster: String, service: String },
}

impl std::fmt::Display for DescriptorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DescriptorError::NotAvailable { cluster, service } => {
                write!(
                    f,
                    "Descriptor not available for service {} at cluster {}",
                    service, cluster
                )
            }
        }
    }
}

impl std::error::Error for DescriptorError {}

enum DescriptorState {
    Missing,
    Pending(u32),
    Resolved(Rc<DescriptorPool>),
}

#[derive(Default)]
pub struct DescriptorManager {
    descriptors: RefCell<HashMap<DescriptorKey, DescriptorState>>,
    descriptor_service: RefCell<Option<String>>,
}

impl DescriptorManager {
    pub fn set_descriptor_service(&self, service: &str) {
        let mut current = self.descriptor_service.borrow_mut();
        if current.as_ref().is_none_or(|s| s != service) {
            *current = Some(service.to_string());
        }
    }

    pub fn add_expected(&self, key: DescriptorKey) {
        self.descriptors
            .borrow_mut()
            .entry(key)
            .or_insert(DescriptorState::Missing);
    }

    pub fn needs_fetch(&self) -> bool {
        self.descriptors
            .borrow()
            .values()
            .any(|state| matches!(state, DescriptorState::Missing))
    }

    pub fn get_pool(
        &self,
        cluster: &str,
        service: &str,
    ) -> Result<Rc<DescriptorPool>, DescriptorError> {
        let key = DescriptorKey::new(cluster.to_string(), service.to_string());
        let descriptors = self.descriptors.borrow();

        match descriptors.get(&key) {
            Some(DescriptorState::Resolved(pool)) => Ok(Rc::clone(pool)),
            _ => Err(DescriptorError::NotAvailable {
                cluster: cluster.to_string(),
                service: service.to_string(),
            }),
        }
    }

    #[cfg(test)]
    pub fn insert_pool(&self, key: DescriptorKey, pool: DescriptorPool) {
        self.descriptors
            .borrow_mut()
            .insert(key, DescriptorState::Resolved(Rc::new(pool)));
    }

    #[cfg(not(test))]
    fn insert_pool(&self, key: DescriptorKey, pool: DescriptorPool) {
        self.descriptors
            .borrow_mut()
            .insert(key, DescriptorState::Resolved(Rc::new(pool)));
    }

    // todo(@adam-cattermole): temporary until manager handles internally
    pub fn contains_pool(&self, key: &DescriptorKey) -> bool {
        matches!(
            self.descriptors.borrow().get(key),
            Some(DescriptorState::Resolved(_))
        )
    }

    fn get_missing(&self) -> Vec<DescriptorKey> {
        self.descriptors
            .borrow()
            .iter()
            .filter_map(|(key, state)| {
                if matches!(state, DescriptorState::Missing) {
                    Some(key.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    pub fn fetch_missing(&self, ctx: &dyn Context) -> Result<(), String> {
        let missing = self.get_missing();

        if missing.is_empty() {
            return Ok(());
        }

        let descriptor_service = self
            .descriptor_service
            .borrow()
            .as_ref()
            .ok_or("descriptor service not configured")?
            .clone();

        debug!(
            "Fetching descriptors for {} missing services: {:?}",
            missing.len(),
            missing
        );

        let request = GetServiceDescriptorsRequest {
            services: missing
                .iter()
                .map(|key| ServiceRef {
                    cluster_name: key.cluster.clone(),
                    service: key.service.clone(),
                })
                .collect(),
        };

        let mut request_bytes = Vec::new();
        request
            .encode(&mut request_bytes)
            .map_err(|e| format!("could not encode descriptor request: {}", e))?;

        let token = ctx
            .dispatch_grpc_call(
                &descriptor_service,
                "kuadrant.v1.DescriptorService",
                "GetServiceDescriptors",
                vec![],
                Some(&request_bytes),
                DESCRIPTOR_FETCH_TIMEOUT,
            )
            .map_err(|status| format!("could not dispatch descriptor fetch: {:?}", status))?;

        debug!(
            "Dispatched descriptor fetch for {} services (token: {})",
            missing.len(),
            token
        );

        let mut descriptors = self.descriptors.borrow_mut();
        for key in missing {
            descriptors.insert(key, DescriptorState::Pending(token));
        }

        Ok(())
    }

    pub fn handle_response(&self, token_id: u32, response_bytes: Vec<u8>) -> Result<(), String> {
        let has_pending = self
            .descriptors
            .borrow()
            .values()
            .any(|state| matches!(state, DescriptorState::Pending(t) if *t == token_id));

        if !has_pending {
            return Err(format!(
                "Received descriptor response for unknown token {}",
                token_id
            ));
        }

        let response = GetServiceDescriptorsResponse::decode(response_bytes.as_slice())
            .map_err(|e| format!("could not decode descriptor response: {}", e))?;

        debug!(
            "Received {} service descriptors",
            response.descriptors.len()
        );

        let errors: Vec<_> = response
            .descriptors
            .into_iter()
            .filter_map(|descriptor| {
                let key = DescriptorKey::new(descriptor.cluster_name, descriptor.service);

                let result = FileDescriptorSet::decode(descriptor.file_descriptor_set.as_slice())
                    .map_err(|e| format!("could not decode FileDescriptorSet for {:?}: {}", key, e))
                    .and_then(|fds| {
                        DescriptorPool::from_file_descriptor_set(fds).map_err(|e| {
                            format!("could not build DescriptorPool for {:?}: {}", key, e)
                        })
                    })
                    .and_then(|pool| {
                        if pool.get_service_by_name(&key.service).is_some() {
                            Ok(pool)
                        } else {
                            Err(format!(
                                "DescriptorPool for {:?} does not contain service {}",
                                key, key.service
                            ))
                        }
                    });

                match result {
                    Ok(pool) => {
                        debug!("Cached descriptor for {:?}", key);
                        self.insert_pool(key, pool);
                        None
                    }
                    Err(e) => {
                        error!("{}", e);
                        Some(e)
                    }
                }
            })
            .collect();

        if !errors.is_empty() {
            return Err(format!("Failed to process {} descriptor(s)", errors.len()));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost_types::{
        field_descriptor_proto, DescriptorProto, FieldDescriptorProto, FileDescriptorProto,
        FileDescriptorSet, MethodDescriptorProto, ServiceDescriptorProto,
    };

    fn create_test_descriptor_pool() -> DescriptorPool {
        let file_descriptor = FileDescriptorProto {
            name: Some("test.proto".to_string()),
            package: Some("test".to_string()),
            message_type: vec![
                DescriptorProto {
                    name: Some("Request".to_string()),
                    field: vec![FieldDescriptorProto {
                        name: Some("id".to_string()),
                        number: Some(1),
                        r#type: Some(field_descriptor_proto::Type::String.into()),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                DescriptorProto {
                    name: Some("Response".to_string()),
                    field: vec![FieldDescriptorProto {
                        name: Some("result".to_string()),
                        number: Some(1),
                        r#type: Some(field_descriptor_proto::Type::String.into()),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
            ],
            service: vec![ServiceDescriptorProto {
                name: Some("TestService".to_string()),
                method: vec![MethodDescriptorProto {
                    name: Some("TestMethod".to_string()),
                    input_type: Some(".test.Request".to_string()),
                    output_type: Some(".test.Response".to_string()),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };

        let fds = FileDescriptorSet {
            file: vec![file_descriptor],
        };

        DescriptorPool::from_file_descriptor_set(fds).expect("Failed to create descriptor pool")
    }

    #[test]
    fn test_get_pool_returns_error_when_not_available() {
        let manager = DescriptorManager::default();

        let result = manager.get_pool("test-cluster", "test.Service");
        assert!(result.is_err());

        assert!(
            matches!(result,  Err(DescriptorError::NotAvailable { cluster, service }) if cluster == "test-cluster" && service == "test.Service")
        );
    }

    #[test]
    fn test_insert_and_get_pool() {
        let manager = DescriptorManager::default();
        let pool = create_test_descriptor_pool();

        let key = DescriptorKey::new("test-cluster".to_string(), "test.TestService".to_string());
        manager.insert_pool(key, pool);

        let result = manager.get_pool("test-cluster", "test.TestService");
        assert!(result.is_ok());

        let retrieved_pool = result.unwrap();
        let service = retrieved_pool.get_service_by_name("test.TestService");
        assert!(service.is_some());
    }
}
