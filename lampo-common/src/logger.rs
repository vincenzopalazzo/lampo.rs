//! Logging module.
///
/// Credit to https://github.com/vincenzopalazzo/nakamoto/blob/master/node/src/logger.rs
use std::io;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::SystemTime;
// FIXME: this is not async we should modify it
use std::fs::File;

use chrono::prelude::*;
use colored::*;

pub use log::{Level, Log, Metadata, Record, SetLoggerError};

struct Logger {
    level: Level,
    file: Option<File>,
}

impl Log for Logger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            let target = record.target();

            if let Some(ref file) = self.file {
                write(record, target, file);
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
                let utc_time: DateTime<Utc> = DateTime::from(SystemTime::now());
                let colored_string = utc_time
                    .to_rfc3339_opts(SecondsFormat::Millis, true)
                    .white();

                writeln!(stream, "{} {}", colored_string, message,).expect("write shouldn't fail");
            }
        }
    }

    fn flush(&self) {}
}

/// Initialize a new logger.
pub fn init(level: &str, file: Option<PathBuf>) -> anyhow::Result<()> {
    let file = if let Some(path) = file {
        Some(File::create(path)?)
    } else {
        None
    };
    let level = Level::from_str(level).map_err(|err| anyhow::anyhow!("{err}"))?;
    let logger = Logger { level, file };

    log::set_boxed_logger(Box::new(logger)).map_err(|err| anyhow::anyhow!("{err}"))?;
    log::set_max_level(level.to_level_filter());

    Ok(())
}
