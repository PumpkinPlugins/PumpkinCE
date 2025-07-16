use crate::SHOULD_STOP;
use crate::cli::rcon::{NO_TTY_OVERRIDE, send_command};
use crate::logging::ReadlineLogWrapper;
use log::{Level, LevelFilter};
use pumpkin_config::advanced_config;
use rustyline_async::{Readline, ReadlineEvent};
use std::io::{IsTerminal, stdin};
use std::str::FromStr;
use std::sync::atomic::Ordering;
use std::sync::{Arc, LazyLock};
use tokio::net::TcpStream;
use tokio::sync::Mutex;

pub fn setup_logger() {
    if let Some((logger_impl, level)) = &*LOGGER_IMPL {
        log::set_logger(logger_impl).unwrap();
        log::set_max_level(*level);
    }
}

pub fn try_drop_rl() {
    if let Some((wrapper, _)) = &*LOGGER_IMPL {
        if let Some(rl) = wrapper.take_readline() {
            let _ = rl;
        }
    }
}

pub async fn setup_stdin_console(_stream: Arc<Mutex<TcpStream>>) {
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    let rt = tokio::runtime::Handle::current();
    std::thread::spawn(move || {
        while !SHOULD_STOP.load(Ordering::Relaxed) {
            let mut line = String::new();
            if let Ok(size) = stdin().read_line(&mut line) {
                // if no bytes were read, we may have hit EOF
                if size == 0 {
                    break;
                }
            } else {
                break;
            };
            if line.is_empty() || line.as_bytes()[line.len() - 1] != b'\n' {
                log::warn!("Console command was not terminated with a newline");
            }
            rt.block_on(tx.send(line.trim().to_string()))
                .expect("Failed to send command to server");
        }
    });
    while !SHOULD_STOP.load(Ordering::Relaxed) {
        if let Some(command) = rx.recv().await {
            log::debug!("Command received: {command:?}");
        }
    }
}

pub async fn setup_console(rl: Readline, stream: Arc<Mutex<TcpStream>>) {
    let mut rl = rl;
    loop {
        match rl.readline().await {
            Ok(ReadlineEvent::Line(line)) => {
                send_command(line.clone(), stream.clone())
                    .await
                    .unwrap_or_else(|e| {
                        log::error!("Failed to run command: {e}");
                    });
                rl.add_history_entry(line).unwrap();
            }
            Ok(ReadlineEvent::Interrupted) => {
                break;
            }
            err => {
                log::error!("Console command loop failed!");
                log::error!("{err:?}");
                break;
            }
        }
    }
    log::debug!("Stopped console commands task");
    if let Some((wrapper, _)) = &*LOGGER_IMPL {
        wrapper.return_readline(rl);
    }
}

pub static LOGGER_IMPL: LazyLock<Option<(ReadlineLogWrapper, LevelFilter)>> = LazyLock::new(|| {
    if advanced_config().logging.enabled {
        let mut config = simplelog::ConfigBuilder::new();

        if advanced_config().logging.timestamp {
            config.set_time_format_custom(time::macros::format_description!(
                "[year]-[month]-[day] [hour]:[minute]:[second]"
            ));
            config.set_time_level(LevelFilter::Error);
        } else {
            config.set_time_level(LevelFilter::Off);
        }

        if !advanced_config().logging.color {
            for level in Level::iter() {
                config.set_level_color(level, None);
            }
        } else {
            // We are technically logging to a file-like object.
            config.set_write_log_enable_colors(true);
        }

        let level = std::env::var("RUST_LOG")
            .ok()
            .as_deref()
            .map(LevelFilter::from_str)
            .and_then(Result::ok)
            .unwrap_or(LevelFilter::Info);

        if advanced_config().commands.use_tty
            && stdin().is_terminal()
            && !NO_TTY_OVERRIDE.load(Ordering::Relaxed)
        {
            match Readline::new("$ ".to_owned()) {
                Ok((rl, stdout)) => {
                    let logger = simplelog::WriteLogger::new(level, config.build(), stdout);
                    Some((ReadlineLogWrapper::new(logger, None, Some(rl)), level))
                }
                Err(e) => {
                    log::warn!(
                        "Failed to initialize console input ({e}); falling back to simple logger"
                    );
                    let logger = simplelog::SimpleLogger::new(level, config.build());
                    Some((ReadlineLogWrapper::new(logger, None, None), level))
                }
            }
        } else {
            let logger = simplelog::SimpleLogger::new(level, config.build());
            Some((ReadlineLogWrapper::new(logger, None, None), level))
        }
    } else {
        None
    }
});
