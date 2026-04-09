use prost_reflect::DescriptorPool;
use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::Hash;
use std::rc::Rc;
use std::time::Duration;

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
    pub fn set_expected(&self, keys: impl IntoIterator<Item = DescriptorKey>) {
        let mut descriptors = self.descriptors.borrow_mut();
        for key in keys {
            descriptors.entry(key).or_insert(DescriptorState::Missing);
        }
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

    // todo(@adam-cattermole): temporary until manager handles internally
    pub fn get_all_pools(&self) -> HashMap<DescriptorKey, DescriptorPool> {
        self.descriptors
            .borrow()
            .iter()
            .filter_map(|(key, state)| {
                if let DescriptorState::Resolved(pool) = state {
                    Some((key.clone(), DescriptorPool::clone(pool)))
                } else {
                    None
                }
            })
            .collect()
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
