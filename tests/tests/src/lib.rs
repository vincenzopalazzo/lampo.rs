#[cfg(test)]
pub mod lampo_cln_tests;
#[cfg(test)]
mod utils;

#[cfg(test)]
use std::sync::Once;

#[cfg(test)]
static INIT: Once = Once::new();

#[cfg(test)]
fn init() {
    // ignore error
    INIT.call_once(|| {
        use lampo_common::logger;

        logger::init("trace", None).expect("initializing logger for the first time");
    });
}
