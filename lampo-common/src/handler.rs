use std::collections::HashMap;
use std::sync::Arc;

use crate::chan;
use crate::error;
use crate::event::Event;
use crate::json;
use crate::jsonrpc::{Request, Result};

pub trait Handler: Send + Sync {
    fn events(&self) -> chan::Receiver<Event>;
    fn emit(&self, event: Event);
}

pub trait ExternalHandler {
    fn handle(&self, req: &Request<json::Value>) -> error::Result<Option<json::Value>>;
}

type Callback<T> = dyn Fn(&T, json::Value) -> Result<json::Value>;

pub struct InMemoryHandler<T> {
    ctx: Arc<T>,
    methods: HashMap<String, Arc<Callback<T>>>,
}

impl<T> InMemoryHandler<T> {
    pub fn new(ctx: Arc<T>) -> Self {
        Self {
            ctx,
            methods: HashMap::new(),
        }
    }

    pub fn add_rpc<F>(&mut self, method: &str, callback: F) -> error::Result<()>
    where
        F: Fn(&T, json::Value) -> Result<json::Value> + 'static,
    {
        self.methods.insert(method.to_owned(), Arc::new(callback));
        Ok(())
    }
}

impl<T> ExternalHandler for InMemoryHandler<T> {
    fn handle(&self, req: &Request<json::Value>) -> error::Result<Option<json::Value>> {
        let callback = self
            .methods
            .get(&req.method)
            .ok_or(error::anyhow!("method `{}` not found", req.method))?;
        let result =
            callback(self.ctx.as_ref(), req.params.clone()).map_err(|err| error::anyhow!(err))?;
        Ok(Some(result))
    }
}
