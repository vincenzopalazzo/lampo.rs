use std::str::FromStr;

use log::{debug, error, info, trace, warn};

use lampo_common::ldk::util::logger::{Logger, Record};

enum LogLevel {
    Info,
    Debug,
    Warn,
    Error,
    Trace,
}

impl FromStr for LogLevel {
    type Err = String;

    // FIXME: check on the rust lightning docs the level sent to the app level
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.to_lowercase();
        let s = s.as_str();
        match s {
            "debug" => Ok(LogLevel::Debug),
            "info" => Ok(LogLevel::Info),
            "warn" => Ok(LogLevel::Warn),
            "error" => Ok(LogLevel::Error),
            "trace" => Ok(LogLevel::Trace),
            _ => Err(format!("Unknown {} level", s)),
        }
    }
}

#[derive(Clone)]
pub struct LampoLogger;

impl LampoLogger {
    /// Create a new instance of the lampo logger with the
    /// information that are provided.
    #[allow(dead_code)]
    pub fn new() -> Self {
        LampoLogger {}
    }

    fn log(&self, log_level: LogLevel, msg: &str) {
        match log_level {
            LogLevel::Debug => debug!(target: "ldk", "{msg}"),
            LogLevel::Info => info!(target: "ldk", "{msg}"),
            LogLevel::Warn => warn!(target: "ldk", "{msg}"),
            LogLevel::Error => error!(target: "ldk", "{msg}"),
            LogLevel::Trace => trace!(target: "ldk", "{msg}"),
        }
    }
}

impl Logger for LampoLogger {
    fn log(&self, record: Record) {
        let raw_log = record.args.to_string();
        let level = record.level.to_string();

        let log = format!(
            "{}:{} {} {}",
            record.module_path, record.line, level, raw_log
        );

        self.log(LogLevel::from_str(level.as_str()).unwrap(), log.as_str());
    }
}
