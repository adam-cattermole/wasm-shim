use crate::util::common::{wasm_module, LOG_LEVEL};
use crate::util::data;
use proxy_wasm_test_framework::tester;
use proxy_wasm_test_framework::types::{BufferType, LogLevel, MetricType, ReturnType};
use serial_test::serial;

pub mod util;

const CONFIG: &str = r#"{
    "descriptorService": "descriptor-service-cluster",
    "services": {
        "dynamic-ratelimit": {
            "type": "dynamic",
            "endpoint": "limitador-cluster",
            "failureMode": "deny",
            "timeout": "5s",
            "grpcService": "test.TestService",
            "grpcMethod": "TestMethod"
        }
    },
    "actionSets": []
}"#;

#[test]
#[serial]
fn it_fetches_descriptors_on_configure() {
    let args = tester::MockSettings {
        wasm_path: wasm_module(),
        quiet: false,
        allow_unexpected: false,
    };
    let mut module = tester::mock(args).unwrap();

    module
        .call_start()
        .execute_and_expect(ReturnType::None)
        .unwrap();

    let root_context = 1;

    module
        .call_proxy_on_context_create(root_context, 0)
        .expect_log(Some(LogLevel::Info), Some("#1 set_root_context"))
        .execute_and_expect(ReturnType::None)
        .unwrap();

    module
        .call_proxy_on_configure(root_context, 0)
        .expect_log(Some(LogLevel::Info), Some("#1 on_configure"))
        .expect_define_metric(Some(MetricType::Counter), Some("kuadrant.configs"))
        .returning(Some(1))
        .expect_define_metric(Some(MetricType::Counter), Some("kuadrant.hits"))
        .returning(Some(2))
        .expect_define_metric(Some(MetricType::Counter), Some("kuadrant.misses"))
        .returning(Some(3))
        .expect_define_metric(Some(MetricType::Counter), Some("kuadrant.allowed"))
        .returning(Some(4))
        .expect_define_metric(Some(MetricType::Counter), Some("kuadrant.denied"))
        .returning(Some(5))
        .expect_define_metric(Some(MetricType::Counter), Some("kuadrant.errors"))
        .returning(Some(6))
        .expect_increment_metric(Some(1), Some(1))
        .expect_get_buffer_bytes(Some(BufferType::PluginConfiguration))
        .returning(Some(CONFIG.as_bytes()))
        .expect_get_log_level()
        .returning(Some(LOG_LEVEL))
        .expect_grpc_call(
            Some("descriptor-service-cluster"),
            Some("kuadrant.v1.DescriptorService"),
            Some("GetServiceDescriptors"),
            None,
            None,
            Some(5000),
        )
        .returning(Ok(42))
        .execute_and_expect(ReturnType::Bool(true))
        .unwrap();

    let response_bytes = data::descriptor_response::TEST_SERVICE;
    module
        .call_proxy_on_grpc_receive(root_context, 42, response_bytes.len() as i32)
        .expect_get_buffer_bytes(Some(BufferType::GrpcReceiveBuffer))
        .returning(Some(response_bytes))
        .execute_and_expect(ReturnType::None)
        .unwrap();

    let http_context = 2;
    module
        .call_proxy_on_context_create(http_context, root_context)
        .expect_get_log_level()
        .returning(Some(LOG_LEVEL))
        .execute_and_expect(ReturnType::None)
        .unwrap();
}
