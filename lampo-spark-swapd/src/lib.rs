//! lampo-spark-swapd as a library, so the binary and the integration
//! tests share one definition of the engine and its legs.
pub mod api;
pub mod engine;
pub mod lampo_leg;
pub mod settings;
pub mod spark_leg;
pub mod store;
pub mod swap;
