use lightning::util::logger::{Logger, Record};
use log::{debug, error, info, trace, warn};
use std::sync::Arc;
use std::{ops::Deref, str::FromStr};

enum LogLevel {
    Info,
    Debug,
    Warn,
    Error,
}

impl FromStr for LogLevel {
    type Err = String;

    // FIXME: check on the rust lightning docs the level sent to the app level
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "debug" => Ok(LogLevel::Debug),
            "info" => Ok(LogLevel::Info),
            "warn" => Ok(LogLevel::Warn),
            "err" => Ok(LogLevel::Error),
            _ => Err(format!("Unknown {} level", s)),
        }
    }
}

#[derive(Clone)]
pub struct LampoLogger;

impl LampoLogger {
    /// Create a new instance of the lampo logger with the
    /// information that are provided.
    fn new() -> Self {
        LampoLogger {}
    }

    fn log(&self, log_level: LogLevel, msg: &str) {
        match log_level {
            LogLevel::Debug => debug!("{msg}"),
            LogLevel::Info => info!("{msg}"),
            LogLevel::Warn => warn!("{msg}"),
            LogLevel::Error => error!("{msg}"),
        }
    }
}

impl Logger for LampoLogger {
    fn log(&self, record: &Record) {
        let raw_log = record.args.to_string();
        let level = record.level.to_string();

        let log = format!(
            "{} {:<5} [{}:{}] {}\n",
            "-1",
            record.level.to_string(),
            record.module_path,
            record.line,
            raw_log
        );

        self.log(LogLevel::from_str(level.as_str()).unwrap(), log.as_str());
    }
}
