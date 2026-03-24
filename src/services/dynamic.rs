use std::time::Duration;

use prost::Message;
use prost_reflect::{DescriptorPool, DynamicMessage};
use tracing::debug;

use super::{Service, ServiceError};
use crate::configuration::FailureMode;
use crate::kuadrant::ReqRespCtx;

pub struct DynamicService {
    upstream_name: String,
    service_name: String,
    method: String,
    timeout: Duration,
    failure_mode: FailureMode,
    descriptor_pool: DescriptorPool,
}

impl DynamicService {
    pub fn new(
        endpoint: String,
        grpc_service: String,
        grpc_method: String,
        timeout: Duration,
        failure_mode: FailureMode,
        descriptor_pool: DescriptorPool,
    ) -> Self {
        Self {
            upstream_name: endpoint,
            service_name: grpc_service,
            method: grpc_method,
            timeout,
            failure_mode,
            descriptor_pool,
        }
    }

    pub fn failure_mode(&self) -> FailureMode {
        self.failure_mode
    }

    #[allow(dead_code)]
    pub fn dispatch_dynamic(
        &self,
        ctx: &mut ReqRespCtx,
        json_message: &str,
    ) -> Result<u32, ServiceError> {
        let service_descriptor = self
            .descriptor_pool
            .get_service_by_name(&self.service_name)
            .ok_or_else(|| {
                ServiceError::Dispatch(format!(
                    "Service '{}' not found in descriptor pool",
                    self.service_name
                ))
            })?;
        let method_descriptor = service_descriptor
            .methods()
            .find(|m| m.name() == self.method)
            .ok_or_else(|| {
                ServiceError::Dispatch(format!(
                    "Method '{}' not found in service '{}'",
                    self.method, self.service_name
                ))
            })?;
        let input_descriptor = method_descriptor.input();
        // todo(@adam-cattermole): To be replaced with CEL construction
        debug!("Deserializing JSON into dynamic message");
        let mut deserializer = serde_json::Deserializer::from_str(json_message);
        let request_message = DynamicMessage::deserialize(input_descriptor, &mut deserializer)
            .map_err(|e| ServiceError::Dispatch(format!("Failed to deserialize JSON: {}", e)))?;
        deserializer
            .end()
            .map_err(|e| ServiceError::Dispatch(format!("JSON deserializer error: {}", e)))?;
        let mut message_bytes = Vec::new();
        request_message
            .encode(&mut message_bytes)
            .map_err(|e| ServiceError::Dispatch(format!("Failed to encode message: {}", e)))?;
        self.dispatch(
            ctx,
            &self.upstream_name,
            &self.service_name,
            &self.method,
            message_bytes,
            self.timeout,
        )
    }
}

impl Service for DynamicService {
    type Response = DynamicMessage;

    fn parse_message(&self, message: Vec<u8>) -> Result<Self::Response, ServiceError> {
        let service_descriptor = self
            .descriptor_pool
            .get_service_by_name(&self.service_name)
            .ok_or_else(|| {
                ServiceError::Decode(format!(
                    "Service '{}' not found in descriptor pool",
                    self.service_name
                ))
            })?;
        let method_descriptor = service_descriptor
            .methods()
            .find(|m| m.name() == self.method)
            .ok_or_else(|| {
                ServiceError::Decode(format!(
                    "Method '{}' not found in service '{}'",
                    self.method, self.service_name
                ))
            })?;
        let output_descriptor = method_descriptor.output();
        let response = DynamicMessage::decode(output_descriptor, message.as_slice())
            .map_err(|e| ServiceError::Decode(format!("Failed to decode response: {}", e)))?;
        Ok(response)
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
                    name: Some("TestRequest".to_string()),
                    field: vec![FieldDescriptorProto {
                        name: Some("message".to_string()),
                        number: Some(1),
                        r#type: Some(field_descriptor_proto::Type::String.into()),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                DescriptorProto {
                    name: Some("TestResponse".to_string()),
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
                    input_type: Some(".test.TestRequest".to_string()),
                    output_type: Some(".test.TestResponse".to_string()),
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
    fn test_dynamic_service_message_building() {
        let pool = create_test_descriptor_pool();
        let service = DynamicService::new(
            "test-cluster".to_string(),
            "test.TestService".to_string(),
            "TestMethod".to_string(),
            Duration::from_secs(1),
            FailureMode::Deny,
            pool,
        );

        let json_request = r#"{ "message": "hello" }"#;

        let service_desc = service
            .descriptor_pool
            .get_service_by_name(&service.service_name)
            .expect("Service not found");
        let method_desc = service_desc
            .methods()
            .find(|m| m.name() == service.method)
            .expect("Method not found");
        let input_desc = method_desc.input();

        let mut deserializer = serde_json::Deserializer::from_str(json_request);
        let message = DynamicMessage::deserialize(input_desc, &mut deserializer)
            .expect("Failed to deserialize");
        deserializer.end().expect("Deserializer should end cleanly");

        let mut bytes = Vec::new();
        message.encode(&mut bytes).expect("Failed to encode");
        assert!(!bytes.is_empty());
    }

    #[test]
    fn test_parse_message_with_valid_response() {
        let pool = create_test_descriptor_pool();
        let service = DynamicService::new(
            "test-cluster".to_string(),
            "test.TestService".to_string(),
            "TestMethod".to_string(),
            Duration::from_secs(1),
            FailureMode::Deny,
            pool,
        );

        let service_desc = service
            .descriptor_pool
            .get_service_by_name(&service.service_name)
            .expect("Service not found");
        let method_desc = service_desc
            .methods()
            .find(|m| m.name() == service.method)
            .expect("Method not found");
        let output_desc = method_desc.output();

        let response_json = r#"{ "result": "success" }"#;
        let mut deserializer = serde_json::Deserializer::from_str(response_json);
        let dynamic_response = DynamicMessage::deserialize(output_desc, &mut deserializer)
            .expect("Failed to deserialize response");

        let mut response_bytes = Vec::new();
        dynamic_response
            .encode(&mut response_bytes)
            .expect("Failed to encode");

        let parsed = service.parse_message(response_bytes);
        assert!(parsed.is_ok());
    }
}
