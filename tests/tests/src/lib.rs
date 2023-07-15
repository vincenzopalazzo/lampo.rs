#[cfg(test)]
pub mod lampo_cln_tests;

#[cfg(test)]
use std::sync::Once;

#[cfg(test)]
static INIT: Once = Once::new();

#[cfg(test)]
fn init() {
    // ignore error
    INIT.call_once(|| {
        use lampo_common::logger;
        use lampo_common::logger::Level;

        logger::init(Level::Trace).expect("initializing logger for the first time");
    });
}
