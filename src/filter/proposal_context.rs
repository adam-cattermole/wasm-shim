use crate::action_set_index::ActionSetIndex;
use crate::filter::proposal_context::no_implicit_dep::{
    GrpcMessageReceiverOperation, GrpcMessageSenderOperation, HeadersOperation, Operation,
};
use crate::runtime_action_set::RuntimeActionSet;
use crate::service::{GrpcErrResponse, GrpcRequest, HeaderResolver};
use log::{debug, warn};
use proxy_wasm::traits::{Context, HttpContext};
use proxy_wasm::types::{Action, Status};
use std::mem;
use std::rc::Rc;

pub mod no_implicit_dep {
    use crate::filter::proposal_context::no_implicit_dep::Operation::SendGrpcRequest;
    use crate::runtime_action_set::RuntimeActionSet;
    use crate::service::{GrpcErrResponse, GrpcRequest, IndexedGrpcRequest};
    use std::rc::Rc;

    pub enum Operation {
        SendGrpcRequest(GrpcMessageSenderOperation),
        AwaitGrpcResponse(GrpcMessageReceiverOperation),
        AddHeaders(HeadersOperation),
        Die(GrpcErrResponse),
        // Done indicates that we have no more operations and can resume the http request flow
        Done(),
    }

    pub struct GrpcMessageSenderOperation {
        runtime_action_set: Rc<RuntimeActionSet>,
        grpc_request: IndexedGrpcRequest,
    }

    impl GrpcMessageSenderOperation {
        pub fn new(
            runtime_action_set: Rc<RuntimeActionSet>,
            grpc_request: IndexedGrpcRequest,
        ) -> Self {
            Self {
                runtime_action_set,
                grpc_request,
            }
        }

        // todo(adam-cattermole): perhaps we should merge these three, return all
        //  but consume self? this avoids the ordering of usage
        pub fn runtime_action_set(&self) -> Rc<RuntimeActionSet> {
            Rc::clone(&self.runtime_action_set)
        }

        pub fn current_index(&self) -> usize {
            self.grpc_request.index()
        }

        pub fn grpc_request(self) -> GrpcRequest {
            self.grpc_request.request()
        }
    }

    pub struct GrpcMessageReceiverOperation {
        runtime_action_set: Rc<RuntimeActionSet>,
        current_index: usize,
    }

    impl GrpcMessageReceiverOperation {
        pub fn new(runtime_action_set: Rc<RuntimeActionSet>, current_index: usize) -> Self {
            Self {
                runtime_action_set,
                current_index,
            }
        }

        pub fn digest_grpc_response(self, msg: &[u8]) -> Vec<Operation> {
            let result = self
                .runtime_action_set
                .process_grpc_response(self.current_index, msg);

            match result {
                Ok((next_msg, headers)) => {
                    let mut operations = Vec::new();
                    if let Some(headers) = headers {
                        operations.push(Operation::AddHeaders(HeadersOperation::new(headers)))
                    }
                    operations.push(match next_msg {
                        None => Operation::Done(),
                        Some(indexed_req) => SendGrpcRequest(GrpcMessageSenderOperation::new(
                            self.runtime_action_set,
                            indexed_req,
                        )),
                    });
                    operations
                }
                Err(grpc_err_resp) => vec![Operation::Die(grpc_err_resp)],
            }
        }

        pub fn fail(self) -> Operation {
            //todo(adam-cattermole): should this take into account failure mode?
            // these errors occurred at filter layer,
            // i.e. error response / failed to read buffer / failed serdes
            Operation::Die(GrpcErrResponse::new_internal_server_error())
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
}

pub(crate) struct Filter {
    context_id: u32,
    index: Rc<ActionSetIndex>,
    header_resolver: Rc<HeaderResolver>,

    grpc_message_receiver_operation: Option<GrpcMessageReceiverOperation>,
    headers_operations: Vec<HeadersOperation>,
}

impl Context for Filter {
    fn on_grpc_call_response(&mut self, _token_id: u32, status_code: u32, resp_size: usize) {
        let receiver = mem::take(&mut self.grpc_message_receiver_operation)
            .expect("We need an operation pending a gRPC response");

        if status_code != Status::Ok as u32 {
            self.handle_operation(receiver.fail());
            return;
        }

        let response_body = match self.get_grpc_call_response_body(0, resp_size) {
            Some(body) => body,
            None => {
                self.handle_operation(receiver.fail());
                return;
            }
        };

        let ops = receiver.digest_grpc_response(&response_body);
        ops.into_iter().for_each(|op| {
            self.handle_operation(op);
        })
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
                let grpc_request = action_set.start_flow();
                let op = match grpc_request {
                    None => Operation::Done(),
                    Some(indexed_req) => Operation::SendGrpcRequest(
                        GrpcMessageSenderOperation::new(Rc::clone(action_set), indexed_req),
                    ),
                };
                return self.handle_operation(op);
            }
        }
        Action::Continue
    }

    fn on_http_response_headers(&mut self, _num_headers: usize, _end_of_stream: bool) -> Action {
        let headers_operations = mem::take(&mut self.headers_operations);
        for op in headers_operations {
            for (header, value) in &op.headers() {
                self.add_http_response_header(header, value)
            }
        }
        Action::Continue
    }
}

impl Filter {
    fn handle_operation(&mut self, operation: Operation) -> Action {
        match operation {
            Operation::SendGrpcRequest(sender_op) => {
                debug!("handle_operation: SendGrpcRequest");
                let next_op = {
                    let index = sender_op.current_index();
                    let action_set = sender_op.runtime_action_set();
                    match self.send_grpc_request(sender_op.grpc_request()) {
                        Ok(_token) => Operation::AwaitGrpcResponse(
                            GrpcMessageReceiverOperation::new(action_set, index),
                        ),
                        Err(_status) => panic!("Error sending request"),
                    }
                };
                self.handle_operation(next_op)
            }
            Operation::AwaitGrpcResponse(receiver_op) => {
                debug!("handle_operation: AwaitGrpcResponse");
                self.grpc_message_receiver_operation = Some(receiver_op);
                Action::Pause
            }
            Operation::AddHeaders(header_op) => {
                debug!("handle_operation: AddHeaders");
                self.headers_operations.push(header_op);
                Action::Continue
            }
            Operation::Die(die_op) => {
                debug!("handle_operation: Die");
                self.die(die_op);
                Action::Continue
            }
            Operation::Done() => {
                debug!("handle_operation: Done");
                self.resume_http_request();
                Action::Continue
            }
        }
    }

    fn die(&mut self, die: GrpcErrResponse) {
        self.send_http_response(
            die.status_code(),
            die.headers(),
            Some(die.body().as_bytes()),
        );
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

    pub fn new(
        context_id: u32,
        index: Rc<ActionSetIndex>,
        header_resolver: Rc<HeaderResolver>,
    ) -> Self {
        Self {
            context_id,
            index,
            header_resolver,
            grpc_message_receiver_operation: None,
            headers_operations: Vec::default(),
        }
    }
}
