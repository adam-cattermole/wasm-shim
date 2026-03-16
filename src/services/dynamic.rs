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
