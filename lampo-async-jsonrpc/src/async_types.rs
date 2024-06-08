use std::future::Future;

use async_trait::async_trait;
use serde_json::Value;

use crate::errors::Error;

#[async_trait]
pub trait AsyncRpcMethod<T>: Send + Sync {
    async fn call(&self, ctx: &T, params: &Value) -> Result<Value, Error>;
}

#[async_trait]
impl<T, F, Fut> AsyncRpcMethod<T> for F
where
    T: std::marker::Sync,
    F: Fn(&T, &Value) -> Fut + Send + Sync,
    Fut: Future<Output = Result<Value, Error>> + Send + 'static,
{
    async fn call(&self, ctx: &T, params: &Value) -> Result<Value, Error> {
        self(ctx, params).await
    }
}
