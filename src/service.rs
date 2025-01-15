pub(crate) mod auth;
pub(crate) mod rate_limit;

use crate::configuration::{FailureMode, Service, ServiceType};
use crate::envoy::StatusCode;
use crate::service::auth::{AUTH_METHOD_NAME, AUTH_SERVICE_NAME};
use crate::service::rate_limit::{RATELIMIT_METHOD_NAME, RATELIMIT_SERVICE_NAME};
use crate::service::TracingHeader::{Baggage, Traceparent, Tracestate};
use proxy_wasm::types::Bytes;
use std::cell::OnceCell;
use std::rc::Rc;
use std::time::Duration;

#[derive(Default, Debug)]
pub struct GrpcService {
    service: Rc<Service>,
    name: &'static str,
    method: &'static str,
}

impl GrpcService {
    pub fn new(service: Rc<Service>) -> Self {
        match service.service_type {
            ServiceType::Auth => Self {
                service,
                name: AUTH_SERVICE_NAME,
                method: AUTH_METHOD_NAME,
            },
            ServiceType::RateLimit => Self {
                service,
                name: RATELIMIT_SERVICE_NAME,
                method: RATELIMIT_METHOD_NAME,
            },
        }
    }

    pub fn get_timeout(&self) -> Duration {
        self.service.timeout.0
    }

    pub fn get_service_type(&self) -> ServiceType {
        self.service.service_type.clone()
    }

    pub fn get_failure_mode(&self) -> FailureMode {
        self.service.failure_mode
    }

    fn endpoint(&self) -> &str {
        &self.service.endpoint
    }
    fn name(&self) -> &str {
        self.name
    }
    fn method(&self) -> &str {
        self.method
    }
    pub fn build_request(&self, message: Option<Vec<u8>>) -> GrpcRequest {
        GrpcRequest::new(
            self.endpoint(),
            self.name(),
            self.method(),
            self.get_timeout(),
            message,
        )
    }
}

pub struct IndexedGrpcRequest {
    index: usize,
    request: GrpcRequest,
}

impl IndexedGrpcRequest {
    pub(crate) fn new(index: usize, request: GrpcRequest) -> Self {
        Self { index, request }
    }

    pub fn index(&self) -> usize {
        self.index
    }

    pub fn request(self) -> GrpcRequest {
        self.request
    }
}

// GrpcRequest contains the information required to make a Grpc Call
pub struct GrpcRequest {
    upstream_name: String,
    service_name: String,
    method_name: String,
    timeout: Duration,
    message: Option<Vec<u8>>,
}

impl GrpcRequest {
    pub fn new(
        upstream_name: &str,
        service_name: &str,
        method_name: &str,
        timeout: Duration,
        message: Option<Vec<u8>>,
    ) -> Self {
        Self {
            upstream_name: upstream_name.to_owned(),
            service_name: service_name.to_owned(),
            method_name: method_name.to_owned(),
            timeout,
            message,
        }
    }

    pub fn upstream_name(&self) -> &str {
        &self.upstream_name
    }

    pub fn service_name(&self) -> &str {
        &self.service_name
    }

    pub fn method_name(&self) -> &str {
        &self.method_name
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    pub fn message(&self) -> Option<&[u8]> {
        self.message.as_deref()
    }
}

#[derive(Debug)]
pub struct GrpcErrResponse {
    status_code: u32,
    response_headers: Vec<(String, String)>,
    body: String,
}

impl GrpcErrResponse {
    pub fn new(status_code: u32, response_headers: Vec<(String, String)>, body: String) -> Self {
        Self {
            status_code,
            response_headers,
            body,
        }
    }

    pub fn new_internal_server_error() -> Self {
        Self {
            status_code: StatusCode::InternalServerError as u32,
            response_headers: Vec::default(),
            body: "Internal Server Error.\n".to_string(),
        }
    }

    pub fn status_code(&self) -> u32 {
        self.status_code
    }

    pub fn headers(&self) -> Vec<(&str, &str)> {
        self.response_headers
            .iter()
            .map(|(header, value)| (header.as_str(), value.as_str()))
            .collect()
    }

    pub fn body(&self) -> &str {
        self.body.as_str()
    }
}

#[derive(Debug)]
pub struct HeaderResolver {
    headers: OnceCell<Vec<(&'static str, Bytes)>>,
}

impl Default for HeaderResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl HeaderResolver {
    pub fn new() -> Self {
        Self {
            headers: OnceCell::new(),
        }
    }

    pub fn get_with_ctx<T: proxy_wasm::traits::HttpContext>(
        &self,
        ctx: &T,
    ) -> &Vec<(&'static str, Bytes)> {
        self.headers.get_or_init(|| {
            let mut headers = Vec::new();
            for header in TracingHeader::all() {
                if let Some(value) = ctx.get_http_request_header_bytes((*header).as_str()) {
                    headers.push(((*header).as_str(), value));
                }
            }
            headers
        })
    }
}

// tracing headers
pub enum TracingHeader {
    Traceparent,
    Tracestate,
    Baggage,
}

impl TracingHeader {
    fn all() -> &'static [Self; 3] {
        &[Traceparent, Tracestate, Baggage]
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Traceparent => "traceparent",
            Tracestate => "tracestate",
            Baggage => "baggage",
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use proxy_wasm::traits::Context;
    use std::collections::HashMap;

    struct MockHost {
        headers: HashMap<&'static str, Bytes>,
    }

    impl MockHost {
        pub fn new(headers: HashMap<&'static str, Bytes>) -> Self {
            Self { headers }
        }
    }

    impl Context for MockHost {}

    impl proxy_wasm::traits::HttpContext for MockHost {
        fn get_http_request_header_bytes(&self, name: &str) -> Option<Bytes> {
            self.headers.get(name).map(|b| b.to_owned())
        }
    }

    #[test]
    fn read_headers() {
        let header_resolver = HeaderResolver::new();

        let headers: Vec<(&str, Bytes)> = vec![("traceparent", b"xyz".to_vec())];
        let mock_host = MockHost::new(headers.iter().cloned().collect::<HashMap<_, _>>());

        let resolver_headers = header_resolver.get_with_ctx(&mock_host);

        headers.iter().zip(resolver_headers.iter()).for_each(
            |((header_one, value_one), (header_two, value_two))| {
                assert_eq!(header_one, header_two);
                assert_eq!(value_one, value_two);
            },
        )
    }
}
