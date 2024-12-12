use crate::action_set_index::ActionSetIndex;
use crate::filter::proposal_context::no_implicit_dep::{EndRequestOperation, HeadersOperation};
use crate::service::{GrpcRequest, HeaderResolver};
use log::warn;
use proxy_wasm::hostcalls;
use proxy_wasm::traits::{Context, HttpContext};
use proxy_wasm::types::{Action, Status};
use std::mem;
use std::rc::Rc;

pub mod no_implicit_dep {
    use crate::runtime_action_set::RuntimeActionSet;
    use crate::service::GrpcRequest;
    use std::rc::Rc;

    #[allow(dead_code)]
    pub enum PendingOperation {
        SendGrpcRequest(GrpcMessageSenderOperation),
        AwaitGrpcResponse(GrpcMessageReceiverOperation),
        AddHeaders(HeadersOperation),
        Die(EndRequestOperation),
        //todo(adam-cattermole): does Done make sense? in this case no PendingOperation
        // instead just Option<PendingOperation>?
        Done(),
    }

    pub struct GrpcMessageSenderOperation {
        runtime_action_set: Rc<RuntimeActionSet>,
        current_index: usize,
    }

    impl GrpcMessageSenderOperation {
        //todo(adam-cattermole): should this return a tuple? alternative?
        pub fn progress(self) -> (Option<GrpcRequest>, PendingOperation) {
            let (index, msg) = self.runtime_action_set.process(self.current_index);
            match msg {
                None => (None, PendingOperation::Done()),
                Some(msg) => (
                    Some(msg),
                    PendingOperation::AwaitGrpcResponse(GrpcMessageReceiverOperation {
                        runtime_action_set: self.runtime_action_set,
                        current_index: index,
                    }),
                ),
            }
        }
    }

    pub struct GrpcMessageReceiverOperation {
        runtime_action_set: Rc<RuntimeActionSet>,
        current_index: usize,
    }

    impl GrpcMessageReceiverOperation {
        pub fn progress(self, _msg: &[u8]) -> PendingOperation {
            let action = self
                .runtime_action_set
                .runtime_actions
                .get(self.current_index)
                .unwrap();

            PendingOperation::Done()
        }

        pub fn fail(self) -> PendingOperation {
            PendingOperation::Die(EndRequestOperation::new_with_status(500))
        }
    }

    pub struct HeadersOperation {
        headers: Vec<(String, String)>,
    }

    impl HeadersOperation {
        pub fn new(headers: Vec<(String, String)>) -> Self {
            Self { headers }
        }
        pub fn headers(self) -> Vec<(String, String)> {
            self.headers
        }
    }

    pub struct EndRequestOperation {
        pub status: u32,
        pub headers: Vec<(String, String)>,
        pub body: Option<String>,
    }

    impl EndRequestOperation {
        pub fn new(status: u32, headers: Vec<(String, String)>, body: Option<String>) -> Self {
            Self {
                status,
                headers,
                body,
            }
        }

        pub fn new_with_status(status: u32) -> Self {
            Self::new(status, Vec::default(), None)
        }

        pub fn headers(&self) -> Vec<(&str, &str)> {
            self.headers
                .iter()
                .map(|(header, value)| (header.as_str(), value.as_str()))
                .collect()
        }

        pub fn body(&self) -> Option<&[u8]> {
            self.body.as_deref().map(|s| s.as_bytes())
        }
    }
}

struct Filter {
    index: ActionSetIndex,
    header_resolver: Rc<HeaderResolver>,

    grpc_message_receiver_operation: Option<no_implicit_dep::GrpcMessageReceiverOperation>,
    headers_operations: Vec<HeadersOperation>,
}

impl Context for Filter {
    fn on_grpc_call_response(&mut self, _token_id: u32, status_code: u32, resp_size: usize) {
        let receiver = mem::take(&mut self.grpc_message_receiver_operation)
            .expect("We need an operation pending a gRPC response");
        let next = if status_code != Status::Ok as u32 {
            match self.get_grpc_call_response_body(0, resp_size) {
                Some(response_body) => receiver.progress(&response_body),
                None => receiver.fail(),
            }
        } else {
            receiver.fail()
        };
        self.handle_operation(next);
    }
}

impl HttpContext for Filter {
    fn on_http_request_headers(&mut self, _: usize, _: bool) -> Action {
        if let Some(action_sets) = self
            .index
            .get_longest_match_action_sets(self.request_authority().as_ref())
        {
            if let Some(action_set) = action_sets
                .iter()
                .find(|action_set| action_set.conditions_apply(/* self */))
            {
                self.handle_operation(action_set.start_flow())
            }
        }
        Action::Continue
    }

    fn on_http_response_headers(&mut self, _num_headers: usize, _end_of_stream: bool) -> Action {
        for op in self.headers_operations.drain(..) {
            for (header, value) in &op.headers() {
                self.add_http_response_header(header, value)
            }
        }
        Action::Continue
    }
}

impl Filter {
    fn handle_operation(&mut self, operation: no_implicit_dep::PendingOperation) {
        match operation {
            no_implicit_dep::PendingOperation::SendGrpcRequest(sender_op) => {
                let (msg, op) = sender_op.progress();
                match msg {
                    None => panic!("invalid state!"),
                    Some(m) => match self.send_grpc_request(m) {
                        Ok(_token) => self.handle_operation(op),
                        Err(_status) => {}
                    },
                }
            }
            no_implicit_dep::PendingOperation::AwaitGrpcResponse(receiver_op) => {
                self.grpc_message_receiver_operation = Some(receiver_op)
            }
            no_implicit_dep::PendingOperation::AddHeaders(header_op) => {
                self.headers_operations.push(header_op)
            }
            no_implicit_dep::PendingOperation::Die(die_op) => self.die(die_op),
            no_implicit_dep::PendingOperation::Done() => (),
        }
    }

    fn die(&mut self, die: EndRequestOperation) {
        self.send_http_response(die.status, die.headers(), die.body());
    }

    fn request_authority(&self) -> String {
        match self.get_http_request_header(":authority") {
            None => {
                warn!(":authority header not found");
                String::new()
            }
            Some(host) => {
                let split_host = host.split(':').collect::<Vec<_>>();
                split_host[0].to_owned()
            }
        }
    }

    fn send_grpc_request(&self, req: GrpcRequest) -> Result<u32, Status> {
        let headers = self
            .header_resolver
            .get_with_ctx(self)
            .iter()
            .map(|(header, value)| (*header, value.as_slice()))
            .collect();

        self.dispatch_grpc_call(
            req.upstream_name(),
            req.service_name(),
            req.method_name(),
            headers,
            req.message(),
            req.timeout(),
        )
    }
}
