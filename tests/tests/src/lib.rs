#[cfg(test)]
pub mod lampo_cln_tests;
#[cfg(test)]
pub mod lampo_tests;
#[cfg(test)]
mod utils;

#[cfg(test)]
use std::sync::Once;

#[cfg(test)]
static INIT: Once = Once::new();

#[cfg(test)]
fn init() {
    let level = std::env::var("TEST_LOG_LEVEL");
    // ignore error
    INIT.call_once(|| match level {
        Ok(level) => {
            if let Err(e) = lampo_common::logger::init(&level, None) {
                eprintln!("Error initializing logger: {}", e);
            } else {
                println!("Logger initialized with level: {}", level);
            }
        }
        Err(e) => {
            eprintln!("Error reading TEST_LOG_LEVEL: {}", e);
        }
    });
}
