# Wasm-shim

[![Rust](https://github.com/Kuadrant/wasm-shim/actions/workflows/rust.yaml/badge.svg)](https://github.com/Kuadrant/wasm-shim/actions/workflows/rust.yaml)
[![FOSSA Status](https://app.fossa.com/api/projects/custom%2B162%2Fgit%2Bgithub.com%2FKuadrant%2Fwasm-shim.svg?type=shield&issueType=license)](https://app.fossa.com/projects/custom%2B162%2Fgit%2Bgithub.com%2FKuadrant%2Fwasm-shim?ref=badge_shield&issueType=license)

A Proxy-Wasm module written in Rust, acting as a shim between Envoy and Limitador.

## Sample configuration

Following is a sample configuration used by the shim.

```yaml
services:
  auth-service:
    type: auth
    endpoint: auth-cluster
    failureMode: deny
    timeout: 10ms
  ratelimit-service:
    type: ratelimit
    endpoint: ratelimit-cluster
    failureMode: allow
actionSets:
  - name: rlp-ns-A/rlp-name-A
    routeRuleConditions:
      hostnames: [ "*.toystore.com" ]
      predicates:
      - request.url_path.startsWith("/get")
      - request.host == "test.toystore.com"
      - request.method == "GET"
    actions:
    - service: auth-service
      scope: auth-scope-a
    - service: ratelimit-service
      scope: ratelimit-scope-a
      predicates:
      - auth.identity.anonymous == true
      data:
      - expression:
          key: my_header
          value: request.headers["My-Custom-Header"]
```

## Features

### CEL Predicates and Expression

`routeRuleConditions`'s `predicate`s are expressed in [Common Expression Language (CEL)](https://cel.dev). `Predicate`s
evaluating to a `bool` value, while `Expression`, used for passing data to a service, evaluate to some `Value`.

These expression can operate on the data made available to them through the Well Known Attributes, see below

#### Conditions, Selectors and Operators (deprecated!)

<details>

While still supported, these will eventually disappear. For now though, you still can express them as such:

```yaml
services:
  auth-service:
    type: auth
    endpoint: auth-cluster
    failureMode: deny
    timeout: 10ms
  ratelimit-service:
    type: ratelimit
    endpoint: ratelimit-cluster
    failureMode: deny
actionSets:
  - name: rlp-ns-A/rlp-name-A
    routeRuleConditions:
      hostnames: [ "*.toystore.com" ]
      matches:
      - selector: request.url_path
        operator: startswith
        value: /get
      - selector: request.host
        operator: eq
        value: test.toystore.com
      - selector: request.method
        operator: eq
        value: GET
    actions:
    - service: ratelimit-service
      scope: rlp-ns-A/rlp-name-A
      conditions: []
      data:
      - selector:
          selector: request.headers.My-Custom-Header
      - static:
          key: admin
          value: "1"
```

```Rust
#[derive(Deserialize, PartialEq, Debug, Clone)]
pub enum WhenConditionOperator {
    #[serde(rename = "eq")]
    EqualOperator,
    #[serde(rename = "neq")]
    NotEqualOperator,
    #[serde(rename = "startswith")]
    StartsWithOperator,
    #[serde(rename = "endswith")]
    EndsWithOperator,
    #[serde(rename = "matches")]
    MatchesOperator,
}
```

Selector of an attribute from the contextual properties provided by kuadrant.
See [Well Known Attributes](#Well-Known-Attributes) for more info about available attributes.

The struct is

```Rust
#[derive(Deserialize, Debug, Clone)]
pub struct SelectorItem {
    // Selector of an attribute from the contextual properties provided by kuadrant
    // during request and connection processing
    pub selector: String,

    // If not set it defaults to `selector` field value as the descriptor key.
    #[serde(default)]
    pub key: Option<String>,

    // An optional value to use if the selector is not found in the context.
    // If not set and the selector is not found in the context, then no data is generated.
    #[serde(default)]
    pub default: Option<String>,
}
```

Selectors are tokenized at each non-escaped occurrence of a separator character `.`.
Example:

```
Input: this.is.a.exam\.ple -> Retuns: ["this", "is", "a", "exam.ple"].
```

Some path segments include dot `.` char in them. For instance envoy filter
names: `envoy.filters.http.header_to_metadata`.
In that particular cases, the dot chat (separator), needs to be escaped.

</details>


### Well Known Attributes

| Attribute                                                                                               | Description                                                                                                                                                                                                                    |
|---------------------------------------------------------------------------------------------------------|--------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| [Envoy Attributes](https://www.envoyproxy.io/docs/envoy/latest/intro/arch_overview/advanced/attributes) | Contextual properties provided by Envoy during request and connection processing                                                                                                                                               |
| `source.remote_address`                                                                                 | This attribute evaluates to the `trusted client address` (IP address without port) as it is being defined by [Envoy Doc](https://www.envoyproxy.io/docs/envoy/latest/configuration/http/http_conn_man/headers#x-forwarded-for) |
| `auth.*`                                                                                                | Data made available by the authentication service to the `ActionSet`'s pipeline                                                                                                                                                |

## Building

Prerequisites:

* Install `wasm32-unknown-unknown` build target

```
rustup target add wasm32-unknown-unknown
```

Build the WASM module

```
make build
```

Build the WASM module in release mode

```
make build BUILD=release
```

## Testing

```
cargo test
```

## Running local development environment (kind)

`docker` is required.

Run local development environment

```sh
make local-setup
```

This deploys a local kubernetes cluster using kind, with the local build of wasm-shim mapped to the envoy container. An
echo API as well as limitador, authorino, and some test policies are configured.

To expose the envoy endpoint run the following:

```sh
kubectl port-forward --namespace kuadrant-system deployment/envoy 8000:8000
```

There is then a single auth action set defined for e2e testing:

* `auth-a` which defines auth is required for requests to `/get` for the `AuthConfig` with `effective-route-1`

```sh
curl -H "Host: test.a.auth.com" http://127.0.0.1:8000/get -i
# HTTP/1.1 401 Unauthorized
```

```sh
curl -H "Host: test.a.auth.com" -H "Authorization: APIKEY IAMALICE" http://127.0.0.1:8000/get -i
# HTTP/1.1 200 OK
```

And some rate limit action sets defined for e2e testing:

* `rlp-a`: Only one data item. Data selector should not generate return any value. Thus, descriptor should be empty and
  rate limiting service should **not** be called.

```sh
curl -H "Host: test.a.rlp.com" http://127.0.0.1:8000/get -i
```

* `rlp-b`: Conditions do not match. Hence, rate limiting service should **not** be called.

```sh
curl -H "Host: test.b.rlp.com" http://127.0.0.1:8000/get -i
```

* `rlp-c`: Descriptor entries from multiple data items should be generated. Hence, rate limiting service should be called.

```sh
curl -H "Host: test.c.rlp.com" -H "x-forwarded-for: 50.0.0.1" -H "My-Custom-Header-01: my-custom-header-value-01" -H "x-dyn-user-id: bob" http://127.0.0.1:8000/get -i
```

Check limitador logs for received descriptor entries.

```sh
kubectl logs -f deployment/limitador-sample -n kuadrant-system
```

The expected descriptor entries:

```
Entry { key: "limit_to_be_activated", value: "1" }
```

```
Entry { key: "source.address", value: "50.0.0.1:0" }
```

```
Entry { key: "request.headers.My-Custom-Header-01", value: "my-custom-header-value-01" }
```

```
Entry { key: "user_id", value: "bob" }
```

* `multi-a` which defines two actions for authenticated ratelimiting.

```sh
curl -H "Host: test.a.multi.com" http://127.0.0.1:8000/get -i
# HTTP/1.1 401 Unauthorized
```

Alice has 5 requests per 10 seconds:
```sh
while :; do curl --write-out '%{http_code}\n' --silent --output /dev/null -H "Authorization: APIKEY IAMALICE" -H "Host: test.a.multi.com" http://127.0.0.1:8000/get | grep -E --color "\b(429)\b|$"; sleep 1; done
```

Bob has 2 requests per 10 seconds:
```sh
while :; do curl --write-out '%{http_code}\n' --silent --output /dev/null -H "Authorization: APIKEY IAMBOB" -H "Host: test.a.multi.com" http://127.0.0.1:8000/get | grep -E --color "\b(429)\b|$"; sleep 1; done
```

To rebuild and deploy to the cluster:

```sh
make build local-rollout
```

Stop and clean up resources:

```sh
make local-cleanup
```

## License

[Apache 2.0 License](LICENSE)

[![FOSSA Status](https://app.fossa.com/api/projects/custom%2B162%2Fgit%2Bgithub.com%2FKuadrant%2Fwasm-shim.svg?type=large&issueType=license)](https://app.fossa.com/projects/custom%2B162%2Fgit%2Bgithub.com%2FKuadrant%2Fwasm-shim?ref=badge_large&issueType=license)
