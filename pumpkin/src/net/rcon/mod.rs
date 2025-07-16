use crate::{SHOULD_STOP, STOP_INTERRUPT, server::Server};
use packet::{ClientboundPacket, Packet, PacketError, ServerboundPacket};
use pumpkin_config::{RCONConfig, advanced_config};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use tokio::sync::Mutex;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    select,
};

mod packet;

pub struct RCONServer;

impl RCONServer {
    pub async fn run(config: RCONConfig, server: Arc<Server>) -> Result<(), std::io::Error> {
        let password: Arc<String> = Arc::new(config.password.clone());
        if password.is_empty() {
            log::warn!("No RCON password set, RCON disabled!");
            return Ok(());
        }

        let listener = tokio::net::TcpListener::bind(config.address).await?;
        let connections = Arc::new(AtomicU32::new(0));

        log::info!(
            "RCON server running on {}",
            listener
                .local_addr()
                .expect("Unable to find running address!")
        );

        while !SHOULD_STOP.load(Ordering::Relaxed) {
            let await_new_client = || async {
                let t1 = listener.accept();
                let t2 = STOP_INTERRUPT.notified();

                select! {
                    client = t1 => Some(client),
                    () = t2 => None,
                }
            };
            // Asynchronously wait for an inbound socket.

            let Some(result) = await_new_client().await else {
                break;
            };
            let (connection, address) = result?;

            if config.max_connections != 0
                && connections.load(Ordering::Relaxed) >= config.max_connections
            {
                log::info!(
                    "RCON ({address}): Client tried to connect wile connection limit is reached"
                );
                drop(connection); // Make sure we drop the connection
                continue;
            }
            // FIXME: Add login timeout per ip to protect against bute force attacks!
            log::debug!("RCON ({address}): Connection opened");

            connections.fetch_add(1, Ordering::Relaxed);
            let mut client = RCONClient::new(connection, address);

            let password = password.clone();
            let server = server.clone();
            let connections = connections.clone();

            tokio::spawn(async move {
                while !client.handle(&server, &password).await {}
                if config.logging.quit {
                    log::info!("RCON ({}): Client disconnected", client.address);
                }
                drop(client); // Make sure we drop the connection
                connections.fetch_sub(1, Ordering::Relaxed);
            });
        }
        Ok(())
    }
}

pub struct RCONClient {
    connection: tokio::net::TcpStream,
    address: SocketAddr,
    logged_in: bool,
    incoming: Vec<u8>,
    closed: bool,
}

impl RCONClient {
    #[must_use]
    pub const fn new(connection: tokio::net::TcpStream, address: SocketAddr) -> Self {
        Self {
            connection,
            address,
            logged_in: false,
            incoming: Vec::new(),
            closed: false,
        }
    }

    /// Returns whether the client is closed or not.
    pub async fn handle(&mut self, server: &Arc<Server>, password: &str) -> bool {
        if !self.closed {
            match self.read_bytes().await {
                // The stream is closed, so we can't reply, so we just close everything.
                Ok(true) => return true,
                Ok(false) => {}
                Err(e) => {
                    log::error!("Could not read packet: {e}");
                    return true;
                }
            }
            // If we get a close here, we might have a reply, which we still want to write.
            let _ = self.poll(server, password).await.map_err(|e| {
                log::error!("RCON error: {e}");
                self.closed = true;
            });
        }
        self.closed
    }

    async fn handle_auth_packet(
        &mut self,
        packet: &Packet,
        config: &RCONConfig,
        password: &str,
    ) -> Result<(), PacketError> {
        if packet.get_body() == password {
            self.send(ClientboundPacket::AuthResponse, packet.get_id(), "")
                .await?;
            if config.logging.logged_successfully {
                log::info!("RCON ({}): Client logged in successfully", self.address);
            }
            self.logged_in = true;
        } else {
            if config.logging.wrong_password {
                log::info!("RCON ({}): Client tried the wrong password", self.address);
            }
            self.send(ClientboundPacket::AuthResponse, -1, "").await?;
            self.closed = true;
        }
        Ok(())
    }

    async fn handle_exec_command_packet(
        &mut self,
        packet: &Packet,
        server: &Arc<Server>,
        config: &RCONConfig,
    ) -> Result<(), PacketError> {
        if !self.logged_in {
            log::warn!(
                "RCON ({}): Client attempted to execute a command while not logged in",
                self.address
            );
            return Ok(());
        }

        let output = Arc::new(Mutex::new(Vec::<String>::new()));

        let server_clone = server.clone();
        let packet_body = packet.get_body().to_owned();
        let output_clone = output.clone();

        log::info!(
            "RCON ({}): Client executed command '{}'",
            self.address,
            packet_body
        );

        let dispatcher = server_clone.command_dispatcher.read().await;
        dispatcher
            .handle_command(
                &mut crate::command::CommandSender::Rcon(output_clone),
                &server_clone,
                &packet_body,
            )
            .await;

        // TODO: further investigate into Multiple packet Responses https://developer.valvesoftware.com/wiki/Source_RCON_Protocol#Multiple-packet_Responses
        let output = output.lock().await.join("\n");

        /*
        if output.len() > 4096-10 {
            log::info!("RCON PACKET TOO_LARGE!!!");
        }
        */

        if config.logging.commands && !output.is_empty() {
            log::info!("RCON ({}): {}", self.address, output);
        }

        self.send(ClientboundPacket::Output, packet.get_id(), output.as_str())
            .await?;

        Ok(())
    }

    async fn poll(&mut self, server: &Arc<Server>, password: &str) -> Result<(), PacketError> {
        let Some(packet) = self.receive_packet().await? else {
            return Ok(());
        };
        let config = &advanced_config().networking.rcon;
        match packet.get_type() {
            ServerboundPacket::Auth => self.handle_auth_packet(&packet, config, password).await?,
            ServerboundPacket::ExecCommand => {
                self.handle_exec_command_packet(&packet, server, config)
                    .await?;
            }
        }
        Ok(())
    }

    async fn read_bytes(&mut self) -> std::io::Result<bool> {
        let mut buf = [0; 1460];
        let n = self.connection.read(&mut buf).await?;
        if n == 0 {
            return Ok(true);
        }
        self.incoming.extend_from_slice(&buf[..n]);
        Ok(false)
    }

    async fn send(
        &mut self,
        packet: ClientboundPacket,
        id: i32,
        body: &str,
    ) -> Result<(), PacketError> {
        let buf = packet.write_buf(id, body);
        self.connection
            .write(&buf)
            .await
            .map_err(PacketError::FailedSend)?;
        Ok(())
    }

    async fn receive_packet(&mut self) -> Result<Option<Packet>, PacketError> {
        Packet::deserialize(&mut self.incoming).await
    }
}
