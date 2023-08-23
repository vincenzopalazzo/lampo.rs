//! Logging module.
///
/// Credit to https://github.com/vincenzopalazzo/nakamoto/blob/master/node/src/logger.rs
use std::{io, time::SystemTime};

use chrono::prelude::*;
use colored::*;
pub use log::{Level, Log, Metadata, Record, SetLoggerError};

struct Logger {
    level: Level,
}

impl Log for Logger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            let target = record.target();

            if record.level() == Level::Error {
                write(record, target, io::stderr());
            } else {
                write(record, target, io::stdout());
            }

            fn write(record: &log::Record, target: &str, mut stream: impl io::Write) {
                let message = format!(
                    "{} {} {}. [{}:{}]",
                    record.level(),
                    target.bold(),
                    record.args(),
                    record.file().unwrap_or_default(),
                    record.line().unwrap_or_default(),
                );
                let message = match record.level() {
                    Level::Error => message.red(),
                    Level::Warn => message.yellow(),
                    Level::Info => message.normal(),
                    Level::Debug => message.dimmed(),
                    Level::Trace => message.cyan().dimmed(),
                };

                writeln!(
                    stream,
                    "{} {}",
                    DateTime::from(SystemTime::now())
                        .to_rfc3339_opts(SecondsFormat::Millis, true)
                        .white(),
                    message,
                )
                .expect("write shouldn't fail");
            }
        }
    }

    fn flush(&self) {}
}

/// Initialize a new logger.
pub fn init(level: Level) -> Result<(), SetLoggerError> {
    let logger = Logger { level };

    log::set_boxed_logger(Box::new(logger))?;
    log::set_max_level(level.to_level_filter());

    Ok(())
}
