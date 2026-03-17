use super::kuadrant_filter::KuadrantFilter;
use crate::configuration::PluginConfiguration;
use crate::kuadrant::PipelineFactory;
use crate::metrics::METRICS;
use crate::{WASM_SHIM_FEATURES, WASM_SHIM_GIT_HASH, WASM_SHIM_PROFILE, WASM_SHIM_VERSION};
use const_format::formatcp;
use prost_reflect::DescriptorPool;
use proxy_wasm::traits::{Context, HttpContext, RootContext};
use proxy_wasm::types::ContextType;
use std::collections::HashMap;
use std::rc::Rc;
use tracing::{debug, error, info};

const WASM_SHIM_HEADER: &str = "Kuadrant wasm module";

pub enum ConfigState {
    Ready,
    AwaitingDescriptors {
        config: PluginConfiguration,
        pending_token: u32,
    },
}

impl Default for ConfigState {
    fn default() -> Self {
        ConfigState::Ready
    }
}

pub struct FilterRoot {
    pub context_id: u32,
    pub pipeline_factory: Rc<PipelineFactory>,
    config_state: ConfigState,
    descriptor_cache: HashMap<(String, String), DescriptorPool>,
}

impl FilterRoot {
    pub fn new(context_id: u32) -> Self {
        Self {
            context_id,
            pipeline_factory: Rc::new(PipelineFactory::default()),
            config_state: ConfigState::default(),
            descriptor_cache: HashMap::new(),
        }
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

                let dynamic_services = config.get_dynamic_services();
                if !dynamic_services.is_empty() {
                    let missing_descriptors: Vec<_> = dynamic_services
                        .iter()
                        .filter(|key| !self.descriptor_cache.contains_key(*key))
                        .cloned()
                        .collect();

                    if !missing_descriptors.is_empty() {
                        // todo(@adam-cattermole): Dispatch descriptor fetch for missing descriptors
                        error!(
                            "Dynamic services require descriptors that are not cached: {:?}",
                            missing_descriptors
                        );
                        return false;
                    }
                }

                match PipelineFactory::try_from_with_descriptors(config, &self.descriptor_cache) {
                    Ok(factory) => {
                        self.pipeline_factory = Rc::new(factory);
                    }
                    Err(err) => {
                        error!("failed to compile plugin config: {:?}", err);
                        return false;
                    }
                }
            }
            Err(e) => {
                log::error!("failed to parse plugin config: {}", e);
                return false;
            }
        }
        true
    }

    fn get_type(&self) -> Option<ContextType> {
        Some(ContextType::HttpContext)
    }
}

impl Context for FilterRoot {}

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
