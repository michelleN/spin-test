use anyhow::{bail, Context};
use tokio::sync::oneshot::error::TryRecvError;
use wasmtime_wasi_http::{
    bindings::http::{incoming_handler::IncomingRequest, types::Scheme},
    body::HyperOutgoingBody,
    WasiHttpView,
};

mod bindings {
    wasmtime::component::bindgen!({
        world: "runner",
        path: "host-wit",
        with: {
            "wasi:io/poll": wasmtime_wasi::bindings::io::poll,
            "wasi:io/error": wasmtime_wasi::bindings::io::error,
            "wasi:io/streams": wasmtime_wasi::bindings::io::streams,
            "wasi:clocks/monotonic-clock": wasmtime_wasi::bindings::clocks::monotonic_clock,
            "wasi:http/types": wasmtime_wasi_http::bindings::http::types,
            "fermyon:spin-test/http-helper/response-receiver": super::ResponseReceiver,
        }
    });
}

/// The `spin-test` runtime
pub struct Runtime {
    store: wasmtime::Store<Data>,
    runner: bindings::Runner,
}

impl Runtime {
    /// Create a new runtime
    pub fn instantiate(manifest: String, composed_component: &[u8]) -> anyhow::Result<Self> {
        if std::env::var("SPIN_TEST_DUMP_COMPOSITION").is_ok() {
            let _ = std::fs::write("composition.wasm", composed_component);
        }
        let engine = wasmtime::Engine::default();
        let mut store = wasmtime::Store::new(&engine, Data::new(manifest));

        let component = wasmtime::component::Component::new(&engine, composed_component)
            .context("composed component was an invalid Wasm component")?;

        let mut linker = wasmtime::component::Linker::new(&engine);
        wasmtime_wasi::command::sync::add_to_linker(&mut linker)
            .context("failed to link to wasi")?;
        wasmtime_wasi_http::bindings::http::types::add_to_linker(&mut linker, |x| x)
            .context("failed to link to wasi-http")?;
        bindings::Runner::add_to_linker(&mut linker, |x| x)
            .context("failed to link to test runner world")?;

        let (runner, _) = bindings::Runner::instantiate(&mut store, &component, &mut linker)
            .context("failed to instantiate test runner world")?;
        Ok(Self { store, runner })
    }

    /// Run the test component
    pub fn run(&mut self) -> anyhow::Result<()> {
        self.runner.call_run(&mut self.store)
    }
}

/// Store specific data
struct Data {
    table: wasmtime_wasi::ResourceTable,
    ctx: wasmtime_wasi::WasiCtx,
    http_ctx: wasmtime_wasi_http::WasiHttpCtx,
    manifest: String,
}

impl Data {
    fn new(manifest: String) -> Self {
        let table = wasmtime_wasi::ResourceTable::new();
        let ctx = wasmtime_wasi::WasiCtxBuilder::new()
            .inherit_stdout()
            .inherit_stderr()
            .build();
        Self {
            table,
            ctx,
            http_ctx: wasmtime_wasi_http::WasiHttpCtx,
            manifest,
        }
    }
}

impl wasmtime_wasi_http::WasiHttpView for Data {
    fn ctx(&mut self) -> &mut wasmtime_wasi_http::WasiHttpCtx {
        &mut self.http_ctx
    }

    fn table(&mut self) -> &mut wasmtime_wasi::ResourceTable {
        &mut self.table
    }
}

impl bindings::RunnerImports for Data {
    fn get_manifest(&mut self) -> wasmtime::Result<String> {
        Ok(self.manifest.clone())
    }
}

impl bindings::fermyon::spin_test::http_helper::Host for Data {
    fn new_request(
        &mut self,
        request: wasmtime::component::Resource<wasmtime_wasi_http::types::HostOutgoingRequest>,
    ) -> wasmtime::Result<wasmtime::component::Resource<IncomingRequest>> {
        let req = self.table.get_mut(&request)?;
        use wasmtime_wasi_http::bindings::http::types::Method;
        let method = match &req.method {
            Method::Get => hyper::Method::GET,
            Method::Head => hyper::Method::HEAD,
            Method::Post => hyper::Method::POST,
            Method::Put => hyper::Method::PUT,
            Method::Delete => hyper::Method::DELETE,
            Method::Connect => hyper::Method::CONNECT,
            Method::Options => hyper::Method::OPTIONS,
            Method::Trace => hyper::Method::TRACE,
            Method::Patch => hyper::Method::PATCH,
            Method::Other(o) => hyper::Method::from_bytes(o.as_bytes())?,
        };
        let scheme = match &req.scheme {
            Some(Scheme::Http) | None => "http",
            Some(Scheme::Https) => "https",
            Some(Scheme::Other(other)) => other,
        };
        let mut builder = hyper::Request::builder().method(method).uri(format!(
            "{}://{}{}",
            scheme,
            req.authority.as_deref().unwrap_or("localhost:3000"),
            req.path_with_query.as_deref().unwrap_or("/")
        ));
        for (name, value) in req.headers.iter() {
            builder = builder.header(name, value);
        }
        let req = builder
            .body(req.body.take().unwrap_or_else(body::empty))
            .unwrap();
        self.new_incoming_request(req)
    }

    fn new_response(
        &mut self,
    ) -> wasmtime::Result<(
        wasmtime::component::Resource<wasmtime_wasi_http::types::HostResponseOutparam>,
        wasmtime::component::Resource<bindings::fermyon::spin_test::http_helper::ResponseReceiver>,
    )> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let outparam = self.new_response_outparam(tx)?;
        let receiver = self.table.push(ResponseReceiver(rx))?;
        Ok((outparam, receiver))
    }
}

impl bindings::fermyon::spin_test::http_helper::HostResponseReceiver for Data {
    fn get(
        &mut self,
        self_: wasmtime::component::Resource<ResponseReceiver>,
    ) -> wasmtime::Result<
        Option<
            wasmtime::component::Resource<
                bindings::fermyon::spin_test::http_helper::OutgoingResponse,
            >,
        >,
    > {
        let receiver = self.table.get_mut(&self_)?;
        let response = match receiver.0.try_recv() {
            Ok(r) => r?,
            Err(TryRecvError::Empty) => return Ok(None),
            Err(TryRecvError::Closed) => {
                bail!("response receiver channel closed because outparam was dropped")
            }
        };
        let (parts, body) = response.into_parts();
        let response = wasmtime_wasi_http::types::HostOutgoingResponse {
            status: parts.status,
            headers: parts.headers,
            body: Some(body),
        };
        Ok(Some(self.table.push(response)?))
    }

    fn drop(
        &mut self,
        rep: wasmtime::component::Resource<ResponseReceiver>,
    ) -> wasmtime::Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

pub struct ResponseReceiver(
    tokio::sync::oneshot::Receiver<
        Result<
            hyper::Response<HyperOutgoingBody>,
            wasmtime_wasi_http::bindings::http::types::ErrorCode,
        >,
    >,
);

impl wasmtime_wasi::WasiView for Data {
    fn table(&mut self) -> &mut wasmtime_wasi::ResourceTable {
        &mut self.table
    }

    fn ctx(&mut self) -> &mut wasmtime_wasi::WasiCtx {
        &mut self.ctx
    }
}

pub mod body {
    use http_body_util::{combinators::BoxBody, BodyExt, Empty};
    use wasmtime_wasi_http::body::HyperIncomingBody;

    pub fn empty() -> HyperIncomingBody {
        BoxBody::new(Empty::new().map_err(|_| unreachable!()))
    }
}