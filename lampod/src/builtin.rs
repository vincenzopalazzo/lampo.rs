//! Build in macros to help to clean up the code
//! but in reality this is hiding just shit!
#[macro_export]
macro_rules! sync {
    ($expr: expr) => {
        Box::pin(async move { $expr })
    };
}
