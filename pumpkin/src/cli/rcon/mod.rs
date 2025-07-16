mod console;
mod packet;

use crate::cli::rcon::MyError::{AuthFailed, PacketIdMismatch, WrongPacket};
use crate::cli::rcon::console::{LOGGER_IMPL, setup_logger, try_drop_rl};
use crate::cli::rcon::packet::{KnownPacketType, Packet, PacketError, PacketToServer};
use pumpkin_config::{RCONConfig, advanced_config};
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use thiserror::Error;
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;
use tokio::sync::Mutex;

fn gen_random_auth_id() -> i32 {
    // We cant use -1 as an id in authentication as -1 represents login failure
    let mut random_number: i32;
    loop {
        random_number = rand::random();
        if random_number != -1 {
            break;
        } else {
            log::info!("You are 1 in 4.29 billion :D")
        }
    }
    random_number
}

#[derive(Debug, Error)]
pub enum MyError {
    #[error("Password incorrect or blocked")]
    AuthFailed,

    #[error("Packet ID mismatch")]
    PacketIdMismatch,

    #[error("Server responded with incorrect packet type")]
    WrongPacket,
}

pub async fn login(rcon_config: &RCONConfig, stream: Arc<Mutex<TcpStream>>) -> Result<(), MyError> {
    let id = gen_random_auth_id();

    log::info!("Authenticating...");

    let mut stream_lock = stream.lock().await;
    Packet::new(
        PacketToServer::Auth as i32,
        rcon_config.password.clone(),
        id,
    )
    .write_to_async(&mut *stream_lock)
    .await
    .unwrap();
    drop(stream_lock);

    let packet = read(stream.clone()).await.unwrap();
    if packet.typ != KnownPacketType::Auth as i32 {
        return Err(WrongPacket);
    }
    if packet.id == -1 {
        return Err(AuthFailed);
    }
    if packet.id != id {
        return Err(PacketIdMismatch);
    }
    log::info!("Successfully authenticated");
    Ok(())
}

pub async fn send_command(command: String, stream: Arc<Mutex<TcpStream>>) -> Result<(), MyError> {
    let id = rand::random();

    let mut stream_lock = stream.lock().await;
    Packet::new(PacketToServer::Command as i32, command, id)
        .write_to_async(&mut *stream_lock)
        .await
        .unwrap();
    drop(stream_lock);

    let packet = read(stream.clone()).await.unwrap();
    if packet.typ != KnownPacketType::Response as i32 {
        return Err(WrongPacket);
    }
    if packet.id != id {
        return Err(PacketIdMismatch);
    }

    log::info!("{}", packet.body);
    Ok(())
}

pub async fn read(stream: Arc<Mutex<TcpStream>>) -> Result<Packet, PacketError> {
    let mut recv_buf = Vec::new();
    let mut stream = stream.lock().await;

    loop {
        // 1024 ?
        let mut temp_buf = vec![0u8; 1024];
        let n = stream.read(&mut temp_buf).await?;

        if n == 0 {
            log::info!("Connection closed");
            return Err(PacketError::InvalidLength);
        }

        recv_buf.extend_from_slice(&temp_buf[..n]);

        if let Some(packet) = Packet::deserialize(&mut recv_buf).await? {
            return Ok(packet);
        }
    }
}

static NO_TTY_OVERRIDE: AtomicBool = AtomicBool::new(false);

pub async fn handle_rcon(host: Option<String>, full_command: String) {
    if !full_command.is_empty() {
        NO_TTY_OVERRIDE.store(true, Ordering::Relaxed);
    }
    setup_logger();

    let rcon_config = &advanced_config().networking.rcon;
    let address: SocketAddr = if let Some(host) = host {
        host.to_socket_addrs()
            .expect("Failed to resolve address")
            .find(|addr| addr.is_ipv4())
            .or_else(|| host.to_socket_addrs().ok()?.next())
            .expect("No addresses found for hostname")
    } else {
        if !rcon_config.enabled {
            log::error!("RCON is disabled!");
            return;
        }
        rcon_config.address
    };

    log::info!("Connecting to '{address}'");
    let stream = Arc::new(Mutex::new(TcpStream::connect(address).await.unwrap()));

    if let Err(e) = login(rcon_config, stream.clone()).await {
        log::error!("Login failed: {e}");
        return;
    }

    if !full_command.is_empty() {
        log::info!("Running Command '{full_command}'");

        if let Err(e) = send_command(full_command, stream.clone()).await {
            log::error!("Failed to run command: {e}");
        }

        try_drop_rl();
        return;
    }

    if advanced_config().commands.use_console {
        if let Some((wrapper, _)) = &*LOGGER_IMPL {
            if let Some(rl) = wrapper.take_readline() {
                console::setup_console(rl, stream.clone()).await;
            } else {
                if advanced_config().commands.use_tty {
                    log::warn!(
                        "The input is not a TTY; falling back to simple logger and ignoring `use_tty` setting"
                    );
                }
                console::setup_stdin_console(stream.clone()).await;
            }
        }
    }
    log::info!("Shutting down...");
    try_drop_rl();
}
