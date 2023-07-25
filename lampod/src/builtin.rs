//! Build in macros to help to clean up the code
//! but in reality this is hiding just shit!
#[macro_export]
macro_rules! async_run {
    ($rt:expr, $expr:expr) => {{
        $rt.block_on($expr)
    }};
    ($expr:expr) => {{
        let rt = tokio::runtime::Runtime::new().unwrap();
        async_run!(rt, $expr)
    }};
}

#[macro_export]
macro_rules! sync {
    ($expr: expr) => {
        Box::pin(async move { $expr })
    };
}
