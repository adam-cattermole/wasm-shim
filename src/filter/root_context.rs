use super::kuadrant_filter::KuadrantFilter;
use crate::configuration::PluginConfiguration;
use crate::envoy::kuadrant::v1::{
    GetServiceDescriptorsRequest, GetServiceDescriptorsResponse, ServiceRef,
};
use crate::kuadrant::PipelineFactory;
use crate::metrics::METRICS;
use crate::{WASM_SHIM_FEATURES, WASM_SHIM_GIT_HASH, WASM_SHIM_PROFILE, WASM_SHIM_VERSION};
use const_format::formatcp;
use prost::Message;
use prost_reflect::DescriptorPool;
use prost_types::FileDescriptorSet;
use proxy_wasm::traits::{Context, HttpContext, RootContext};
use proxy_wasm::types::ContextType;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;
use tracing::{debug, error, info};

const WASM_SHIM_HEADER: &str = "Kuadrant wasm module";

pub struct FilterRoot {
    pub context_id: u32,
    pub pipeline_factory: Rc<PipelineFactory>,
    pending_requests: HashMap<u32, HashSet<(String, String)>>,
    pending_config: Option<PluginConfiguration>,
    descriptor_cache: HashMap<(String, String), DescriptorPool>,
}

impl FilterRoot {
    pub fn new(context_id: u32) -> Self {
        Self {
            context_id,
            pipeline_factory: Rc::new(PipelineFactory::default()),
            pending_requests: HashMap::new(),
            pending_config: None,
            descriptor_cache: HashMap::new(),
        }
    }

    fn fetch_descriptors(
        &mut self,
        descriptor_service: &str,
        services: Vec<(String, String)>,
    ) -> Result<u32, String> {
        debug!(
            "Configuration requires descriptors for dynamic services: {:?}",
            services
        );

        let request = GetServiceDescriptorsRequest {
            services: services
                .iter()
                .map(|(cluster_name, service)| ServiceRef {
                    cluster_name: cluster_name.clone(),
                    service: service.clone(),
                })
                .collect(),
        };

        let mut request_bytes = Vec::new();
        request
            .encode(&mut request_bytes)
            .map_err(|e| format!("could not encode descriptor request: {}", e))?;

        let token = self
            .dispatch_grpc_call(
                descriptor_service,
                "kuadrant.v1.DescriptorService",
                "GetServiceDescriptors",
                vec![],
                Some(&request_bytes),
                Duration::from_secs(5),
            )
            .map_err(|status| format!("could not dispatch descriptor fetch: {:?}", status))?;

        debug!(
            "Configuration pending: fetching descriptors for {} services (token: {})",
            services.len(),
            token
        );

        Ok(token)
    }

    fn process_descriptor_response(&mut self, response_bytes: Vec<u8>) -> Result<(), String> {
        let response = GetServiceDescriptorsResponse::decode(response_bytes.as_slice())
            .map_err(|e| format!("could not decode descriptor response: {}", e))?;

        debug!(
            "Configuration: received {} service descriptors",
            response.descriptors.len()
        );

        for descriptor in response.descriptors {
            let key = (descriptor.cluster_name, descriptor.service);
            let fds = FileDescriptorSet::decode(descriptor.file_descriptor_set.as_slice())
                .map_err(|e| format!("could not decode FileDescriptorSet for {:?}: {}", key, e))?;
            let pool = DescriptorPool::from_file_descriptor_set(fds)
                .map_err(|e| format!("could not build DescriptorPool for {:?}: {}", key, e))?;
            debug!("Configuration: cached descriptor for {:?}", key);
            self.descriptor_cache.insert(key, pool);
        }

        Ok(())
    }

    fn activate_config(&mut self, config: PluginConfiguration) -> bool {
        match PipelineFactory::try_from_with_descriptors(config, &self.descriptor_cache) {
            Ok(factory) => {
                self.pipeline_factory = Rc::new(factory);
                true
            }
            Err(err) => {
                error!("failed to compile plugin config: {:?}", err);
                false
            }
        }
    }

    fn process_config(&mut self, config: PluginConfiguration) -> bool {
        let dynamic_services = config.get_dynamic_services();
        if dynamic_services.is_empty() {
            return self.activate_config(config);
        }

        let missing_descriptors: Vec<_> = dynamic_services
            .iter()
            .filter(|key| !self.descriptor_cache.contains_key(*key))
            .cloned()
            .collect();

        if missing_descriptors.is_empty() {
            return self.activate_config(config);
        }

        let in_flight: HashSet<_> = self.pending_requests.values().flatten().cloned().collect();

        let new_services: Vec<_> = missing_descriptors
            .into_iter()
            .filter(|key| !in_flight.contains(key))
            .collect();

        if !new_services.is_empty() {
            match self.fetch_descriptors(&config.descriptor_service, new_services.clone()) {
                Ok(token) => {
                    self.pending_requests
                        .insert(token, new_services.into_iter().collect());
                }
                Err(e) => {
                    error!("Configuration failed: {}", e);
                    return false;
                }
            }
        }

        self.pending_config = Some(config);
        true
    }

    fn handle_descriptor_response(
        &mut self,
        token_id: u32,
        status_code: u32,
        response_size: usize,
    ) -> Result<(), String> {
        if self.pending_requests.remove(&token_id).is_none() {
            debug!("Ignoring grpc response for token {}", token_id);
            return Ok(());
        }

        if status_code != 0 {
            return Err(format!("descriptor fetch returned status {}", status_code));
        }

        let response_bytes = self
            .get_grpc_call_response_body(0, response_size)
            .map_err(|status| format!("could not get descriptor response: {:?}", status))?
            .ok_or_else(|| "descriptor response body is empty".to_string())?;

        self.process_descriptor_response(response_bytes)?;

        if let Some(config) = self.pending_config.take() {
            let missing: Vec<_> = config
                .get_dynamic_services()
                .into_iter()
                .filter(|key| !self.descriptor_cache.contains_key(key))
                .collect();

            if missing.is_empty() {
                self.activate_config(config);
            } else {
                self.pending_config = Some(config);
            }
        }

        Ok(())
    }
}

impl RootContext for FilterRoot {
    fn on_vm_start(&mut self, _vm_configuration_size: usize) -> bool {
        let full_version: &'static str = formatcp!(
            "v{WASM_SHIM_VERSION} ({WASM_SHIM_GIT_HASH}) {WASM_SHIM_FEATURES} {WASM_SHIM_PROFILE}"
        );

        opentelemetry::global::set_text_map_propagator(
            opentelemetry::propagation::TextMapCompositePropagator::new(vec![
                Box::new(opentelemetry_sdk::propagation::TraceContextPropagator::new()),
                Box::new(opentelemetry_sdk::propagation::BaggagePropagator::new()),
            ]),
        );

        log::info!(
            "#{} {} {}: VM started",
            self.context_id,
            WASM_SHIM_HEADER,
            full_version
        );
        true
    }

    fn create_http_context(&self, context_id: u32) -> Option<Box<dyn HttpContext>> {
        crate::tracing::update_log_level();
        debug!("#{} create_http_context", context_id);
        Some(Box::new(KuadrantFilter::new(
            context_id,
            Rc::clone(&self.pipeline_factory),
        )))
    }

    fn on_configure(&mut self, _config_size: usize) -> bool {
        log::info!("#{} on_configure", self.context_id);
        METRICS.configs().increment();
        let configuration: Vec<u8> = match self.get_plugin_configuration() {
            Ok(cfg) => match cfg {
                Some(c) => c,
                None => return false,
            },
            Err(status) => {
                log::error!("#{} on_configure: {:?}", self.context_id, status);
                return false;
            }
        };
        match serde_json::from_slice::<PluginConfiguration>(&configuration) {
            Ok(config) => {
                let use_tracing_exporter = config.observability.tracing.is_some();
                crate::tracing::init_observability(
                    use_tracing_exporter,
                    config.observability.default_level.as_deref(),
                );

                info!("plugin config parsed: {:?}", config);
                self.process_config(config)
            }
            Err(e) => {
                log::error!("failed to parse plugin config: {}", e);
                false
            }
        }
    }

    fn get_type(&self) -> Option<ContextType> {
        Some(ContextType::HttpContext)
    }
}

impl Context for FilterRoot {
    fn on_grpc_call_response(&mut self, token_id: u32, status_code: u32, response_size: usize) {
        if let Err(e) = self.handle_descriptor_response(token_id, status_code, response_size) {
            error!("Configuration failed: {}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::configuration::PluginConfiguration;

    #[test]
    fn invalid_json_fails_to_parse() {
        let invalid_json = "{ invalid json }";
        let result = serde_json::from_slice::<PluginConfiguration>(invalid_json.as_bytes());
        assert!(result.is_err());
    }

    #[test]
    fn config_with_invalid_predicate_fails_factory_creation() {
        let config_str = serde_json::json!({
            "services": {
                "test-service": {
                    "type": "auth",
                    "endpoint": "test-cluster",
                    "failureMode": "deny",
                    "timeout": "5s"
                }
            },
            "actionSets": [{
                "name": "test-action-set",
                "routeRuleConditions": {
                    "hostnames": ["example.com"],
                    "predicates": ["invalid syntax !!!"]
                },
                "actions": []
            }]
        })
        .to_string();

        let config = serde_json::from_slice::<PluginConfiguration>(config_str.as_bytes()).unwrap();
        let result = PipelineFactory::try_from(config);
        assert!(result.is_err());
    }
}
