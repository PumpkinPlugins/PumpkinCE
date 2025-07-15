use std::{
    collections::HashMap,
    io::{Cursor, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering},
    },
};

use bytes::Bytes;
use pumpkin_config::networking::compression::CompressionInfo;
use pumpkin_protocol::{
    ClientPacket, PacketDecodeError, PacketEncodeError, RawPacket, ServerPacket,
    bedrock::{
        RAKNET_ACK, RAKNET_GAME_PACKET, RAKNET_NACK, RAKNET_VALID, RakReliability, SubClient,
        ack::Ack,
        frame_set::{Frame, FrameSet},
        packet_decoder::UDPNetworkDecoder,
        packet_encoder::UDPNetworkEncoder,
        server::{
            login::SLogin,
            raknet::{
                connection::{
                    SConnectedPing, SConnectionRequest, SDisconnect, SNewIncomingConnection,
                },
                open_connection::{SOpenConnectionRequest1, SOpenConnectionRequest2},
                unconnected_ping::SUnconnectedPing,
            },
            request_network_settings::SRequestNetworkSettings,
        },
    },
    codec::u24::U24,
    packet::Packet,
    ser::{NetworkReadExt, NetworkWriteExt, ReadingError, WritingError},
};
use pumpkin_util::text::TextComponent;
use std::net::SocketAddr;
use tokio::{
    net::UdpSocket,
    sync::mpsc::{Receiver, Sender},
    sync::{Mutex, Notify},
    task::JoinHandle,
};
use tokio_util::task::TaskTracker;

pub mod connection;
pub mod login;
pub mod open_connection;
pub mod unconnected;

use crate::{entity::player::Player, server::Server};

pub struct BedrockClientPlatform {
    socket: Arc<UdpSocket>,
    /// The client's IP address.
    pub address: SocketAddr,
    /// The minecraft protocol version used by the client.
    pub protocol_version: AtomicI32,
    pub player: Mutex<Option<Arc<Player>>>,

    tasks: TaskTracker,
    outgoing_packet_queue_send: Sender<Bytes>,
    /// A queue of serialized packets to send to the network
    outgoing_packet_queue_recv: Option<Receiver<Bytes>>,

    /// The packet encoder for outgoing packets.
    network_writer: Arc<Mutex<UDPNetworkEncoder>>,
    /// The packet decoder for incoming packets.
    network_reader: Mutex<UDPNetworkDecoder>,

    use_frame_sets: AtomicBool,
    output_sequence_number: AtomicU32,
    output_reliable_number: AtomicU32,
    output_sequenced_index: AtomicU32,
    output_ordered_index: AtomicU32,

    /// An notifier that is triggered when this client is closed.
    close_interrupt: Arc<Notify>,

    /// Indicates if the client connection is closed.
    pub closed: Arc<AtomicBool>,

    /// Store Fragments until the packet is complete
    compounds: Arc<Mutex<HashMap<u16, Vec<Option<Frame>>>>>,
    //input_sequence_number: AtomicU32,
}

impl BedrockClientPlatform {
    #[must_use]
    pub fn new(socket: Arc<UdpSocket>, address: SocketAddr) -> Self {
        let (send, recv) = tokio::sync::mpsc::channel(128);
        Self {
            socket,
            protocol_version: AtomicI32::new(0),
            player: Mutex::new(None),
            address,
            network_writer: Arc::new(Mutex::new(UDPNetworkEncoder::new())),
            network_reader: Mutex::new(UDPNetworkDecoder::new()),
            tasks: TaskTracker::new(),
            outgoing_packet_queue_send: send,
            outgoing_packet_queue_recv: Some(recv),
            use_frame_sets: AtomicBool::new(false),
            output_sequence_number: AtomicU32::new(0),
            output_reliable_number: AtomicU32::new(0),
            output_sequenced_index: AtomicU32::new(0),
            output_ordered_index: AtomicU32::new(0),
            compounds: Arc::new(Mutex::new(HashMap::new())),
            closed: Arc::new(AtomicBool::new(false)),
            close_interrupt: Arc::new(Notify::new()),
            //input_sequence_number: AtomicU32::new(0),
        }
    }

    pub fn start_outgoing_packet_task(&mut self) {
        let mut packet_receiver = self
            .outgoing_packet_queue_recv
            .take()
            .expect("This was set in the new fn");
        let close_interrupt = self.close_interrupt.clone();
        let closed = self.closed.clone();
        let writer = self.network_writer.clone();
        let addr = self.address;
        let socket = self.socket.clone();
        self.spawn_task(async move {
            while !closed.load(Ordering::Relaxed) {
                let recv_result = tokio::select! {
                    () = close_interrupt.notified() => {
                        None
                    },
                    recv_result = packet_receiver.recv() => {
                        recv_result
                    }
                };

                let Some(packet_data) = recv_result else {
                    break;
                };

                if let Err(err) = writer
                    .lock()
                    .await
                    .write_packet(packet_data, addr, &socket)
                    .await
                {
                    // It is expected that the packet will fail if we are closed
                    if !closed.load(Ordering::Relaxed) {
                        log::warn!("Failed to send packet to client: {err}",);
                        // We now need to close the connection to the client since the stream is in an
                        // unknown state
                        Self::thread_safe_close(&close_interrupt, &closed);
                        break;
                    }
                }
            }
        });
    }

    fn thread_safe_close(interrupt: &Arc<Notify>, closed: &Arc<AtomicBool>) {
        interrupt.notify_waiters();
        closed.store(true, Ordering::Relaxed);
    }

    pub async fn process_packet(self: &Arc<Self>, server: &Server, packet: Cursor<Vec<u8>>) {
        let packet = self.get_packet_payload(packet).await;
        if let Some(packet) = packet {
            if let Err(error) = self.handle_packet_payload(server, packet).await {
                let _text = format!("Error while reading incoming packet {error}");
                log::error!("Failed to read incoming packet with : {error}");
                self.close();
                //self.kick(TextComponent::text(text)).await;
            }
        }
    }

    pub async fn set_compression(&self, compression: CompressionInfo) {
        self.network_reader
            .lock()
            .await
            .set_compression(compression.threshold as usize);

        self.network_writer
            .lock()
            .await
            .set_compression((compression.threshold as usize, compression.level));
    }

    #[allow(clippy::unused_async)]
    pub async fn kick(&self, _reason: TextComponent) {
        // TODO
    }

    pub async fn enqueue_packet<P>(&self, packet: &P)
    where
        P: ClientPacket,
    {
        let mut buf = Vec::new();
        let writer = &mut buf;
        Self::write_raw_packet(packet, writer).unwrap();
        self.enqueue_packet_data(buf.into()).await;
    }

    /// Queues a clientbound packet to be sent to the connected client. Queued chunks are sent
    /// in-order to the client
    ///
    /// # Arguments
    ///
    /// * `packet`: A reference to a packet object implementing the `ClientPacket` trait.
    pub async fn enqueue_packet_data(&self, packet_data: Bytes) {
        if let Err(err) = self.outgoing_packet_queue_send.send(packet_data).await {
            // This is expected to fail if we are closed
            if !self.closed.load(Ordering::Relaxed) {
                log::error!("Failed to add packet to the outgoing packet queue for client: {err}");
            }
        }
    }

    pub fn write_raw_packet<P: ClientPacket>(
        packet: &P,
        mut write: impl Write,
    ) -> Result<(), WritingError> {
        write.write_u8(P::PACKET_ID as u8)?;
        packet.write_packet_data(write)
    }

    pub async fn write_game_packet<P: ClientPacket>(
        &self,
        packet: &P,
        write: impl Write,
    ) -> Result<(), WritingError> {
        let mut packet_payload = Vec::new();
        packet.write_packet_data(&mut packet_payload)?;

        // TODO
        self.network_writer
            .lock()
            .await
            .write_game_packet(
                P::PACKET_ID as u16,
                SubClient::Main,
                SubClient::Main,
                packet_payload.into(),
                write,
            )
            .await
            .unwrap();
        Ok(())
    }

    pub async fn write_packet_data(&self, packet_data: Bytes) -> Result<(), PacketEncodeError> {
        self.network_writer
            .lock()
            .await
            .write_packet(packet_data, self.address, &self.socket)
            .await
    }

    pub async fn send_raknet_packet_now<P: ClientPacket>(&self, packet: &P) {
        let mut packet_buf = Vec::new();
        let writer = &mut packet_buf;
        Self::write_raw_packet(packet, writer).unwrap();
        self.send_packet_now(packet_buf).await;
    }

    pub async fn send_game_packet<P: ClientPacket>(&self, packet: &P, reliability: RakReliability) {
        let mut packet_buf = Vec::new();
        self.write_game_packet(packet, &mut packet_buf)
            .await
            .unwrap();
        self.send_framed_packet_data(packet_buf, reliability).await;
    }

    pub async fn send_framed_packet<P: ClientPacket>(
        &self,
        packet: &P,
        reliability: RakReliability,
    ) {
        let mut packet_buf = Vec::new();
        Self::write_raw_packet(packet, &mut packet_buf).unwrap();
        self.send_framed_packet_data(packet_buf, reliability).await;
    }

    pub async fn send_framed_packet_data(&self, packet_buf: Vec<u8>, reliability: RakReliability) {
        let mut frame_set = FrameSet {
            sequence: U24(self.output_sequence_number.fetch_add(1, Ordering::Relaxed)),
            frames: Vec::with_capacity(1),
        };
        let mut frame = Frame {
            payload: packet_buf.into(),
            reliability,
            ..Default::default()
        };

        if reliability.is_reliable() {
            frame.reliable_number = self.output_reliable_number.fetch_add(1, Ordering::Relaxed);
            if matches!(reliability, RakReliability::ReliableOrdered) {
                //Todo! Check if Fragmenting is needed
            }
        }

        if reliability.is_ordered() {
            frame.order_index = self.output_ordered_index.fetch_add(1, Ordering::Relaxed);
        }

        if reliability.is_sequenced() {
            frame.sequence_index = self.output_sequenced_index.fetch_add(1, Ordering::Relaxed);
        }

        frame_set.frames.push(frame);

        let mut packet_buf = Vec::new();
        frame_set.write_packet_data(&mut packet_buf).unwrap();

        if let Err(err) = self
            .network_writer
            .lock()
            .await
            .write_packet(packet_buf.into(), self.address, &self.socket)
            .await
        {
            // It is expected that the packet will fail if we are closed
            if !self.closed.load(Ordering::Relaxed) {
                log::warn!("Failed to send packet to client: {err}");
                // We now need to close the connection to the client since the stream is in an
                // unknown state
                self.close();
            }
        }
    }

    pub async fn send_packet_now(&self, packet: Vec<u8>) {
        if !self.use_frame_sets.load(Ordering::Relaxed) {
            // Sent the packet directly
            if let Err(err) = self
                .network_writer
                .lock()
                .await
                .write_packet(packet.into(), self.address, &self.socket)
                .await
            {
                // It is expected that the packet will fail if we are closed
                if !self.closed.load(Ordering::Relaxed) {
                    log::warn!("Failed to send packet to client: {err}");
                    // We now need to close the connection to the client since the stream is in an
                    // unknown state
                    self.close();
                }
            }
        }
    }

    pub async fn await_tasks(&self) {
        self.tasks.close();
        self.tasks.wait().await;
    }

    pub fn close(&self) {
        self.close_interrupt.notify_waiters();
        self.closed.store(true, Ordering::Relaxed);
    }

    pub async fn send_ack(&self, packet: &Ack) {
        let mut packet_buf = Vec::new();
        packet_buf.write_u8(0xC0).unwrap();
        packet.write_packet_data(&mut packet_buf).unwrap();

        if let Err(err) = self
            .network_writer
            .lock()
            .await
            .write_packet(packet_buf.into(), self.address, &self.socket)
            .await
        {
            // It is expected that the packet will fail if we are closed
            if !self.closed.load(Ordering::Relaxed) {
                log::warn!("Failed to send packet to client: {err}");
                // We now need to close the connection to the client since the stream is in an
                // unknown state
                self.close();
            }
        }
    }

    pub async fn handle_packet_payload(
        self: &Arc<Self>,
        server: &Server,
        packet: Bytes,
    ) -> Result<(), ReadingError> {
        let mut payload = &packet[..];

        let Ok(id) = payload.get_u8() else {
            return Err(ReadingError::CleanEOF(String::new()));
        };

        let is_valid = id & RAKNET_VALID == RAKNET_VALID;
        if !is_valid {
            // Offline packets just have Packet ID + Payload
            return self
                .handle_offline_packet(server, i32::from(id), payload)
                .await;
        }
        self.use_frame_sets.store(true, Ordering::Relaxed);

        match id {
            RAKNET_ACK => {
                Self::handle_ack(&Ack::read(payload)?);
            }
            RAKNET_NACK => {
                dbg!("received nack, client is missing packets");
            }
            0x80..0x8d => {
                self.handle_frame_set(server, FrameSet::read(payload)?)
                    .await;
            }
            _ => {
                log::warn!("Bedrock: Received unknown packet header {id}");
            }
        }
        Ok(())
    }

    fn handle_ack(_ack: &Ack) {}

    async fn handle_frame_set(self: &Arc<Self>, server: &Server, frame_set: FrameSet) {
        // TODO: Send all ACKs in short intervals in batches
        self.send_ack(&Ack::new(vec![frame_set.sequence.0])).await;
        // TODO
        for frame in frame_set.frames {
            self.handle_frame(server, frame).await.unwrap();
        }
    }

    async fn handle_frame(
        self: &Arc<Self>,
        server: &Server,
        mut frame: Frame,
    ) -> Result<(), ReadingError> {
        if frame.split_size > 0 {
            let fragment_index = frame.split_index as usize;
            let compound_id = frame.split_id;
            let mut compounds = self.compounds.lock().await;

            let entry = compounds.entry(compound_id).or_insert_with(|| {
                let mut vec = Vec::with_capacity(frame.split_size as usize);
                vec.resize_with(frame.split_size as usize, || None);
                vec
            });

            entry[fragment_index] = Some(frame);

            // Check if all fragments are received
            if entry.iter().any(Option::is_none) {
                return Ok(());
            }

            dbg!("compound complete! size", entry.len());
            let mut frames = compounds.remove(&compound_id).unwrap();

            // Safety: We already checked that all frames are Some at this point
            let len = frames
                .iter()
                .map(|frame| unsafe { frame.as_ref().unwrap_unchecked().payload.len() })
                .sum();

            let mut merged = Vec::with_capacity(len);

            for frame in &frames {
                merged.extend_from_slice(unsafe { &frame.as_ref().unwrap_unchecked().payload });
            }

            frame = unsafe { frames[0].take().unwrap_unchecked() };

            frame.payload = merged.into();
            frame.split_size = 0;
        }

        let mut payload = &frame.payload[..];
        let id = payload.get_u8()?;
        self.handle_raknet_packet(server, i32::from(id), payload)
            .await
    }

    async fn handle_game_packet(
        self: &Arc<Self>,
        server: &Server,
        packet: RawPacket,
    ) -> Result<(), ReadingError> {
        let payload = &packet.payload[..];
        match packet.id {
            SRequestNetworkSettings::PACKET_ID => {
                self.handle_request_network_settings(SRequestNetworkSettings::read(payload)?)
                    .await;
            }
            SLogin::PACKET_ID => {
                self.handle_login(SLogin::read(payload)?, server).await;
            }
            _ => {
                log::warn!("Bedrock: Received Unknown Game packet: {}", packet.id);
            }
        }
        Ok(())
    }

    async fn handle_raknet_packet(
        self: &Arc<Self>,
        server: &Server,
        packet_id: i32,
        payload: &[u8],
    ) -> Result<(), ReadingError> {
        match packet_id {
            SConnectionRequest::PACKET_ID => {
                self.handle_connection_request(SConnectionRequest::read(payload)?)
                    .await;
            }
            SNewIncomingConnection::PACKET_ID => {
                self.handle_new_incoming_connection(&SNewIncomingConnection::read(payload)?);
            }
            SConnectedPing::PACKET_ID => {
                self.handle_connected_ping(SConnectedPing::read(payload)?)
                    .await;
            }
            SDisconnect::PACKET_ID => {
                dbg!("Bedrock client disconnected");
                self.close();
            }

            RAKNET_GAME_PACKET => {
                let game_packet = self
                    .network_reader
                    .lock()
                    .await
                    .get_game_packet(Cursor::new(payload.to_vec()))
                    .await
                    .unwrap();

                self.handle_game_packet(server, game_packet).await?;
            }
            _ => {
                log::warn!("Bedrock: Received Unknown RakNet Online packet: {packet_id}");
            }
        }
        Ok(())
    }

    async fn handle_offline_packet(
        &self,
        server: &Server,
        packet_id: i32,
        payload: &[u8],
    ) -> Result<(), ReadingError> {
        match packet_id {
            SUnconnectedPing::PACKET_ID => {
                self.handle_unconnected_ping(server, SUnconnectedPing::read(payload)?)
                    .await;
            }
            SOpenConnectionRequest1::PACKET_ID => {
                self.handle_open_connection_1(server, SOpenConnectionRequest1::read(payload)?)
                    .await;
            }
            SOpenConnectionRequest2::PACKET_ID => {
                self.handle_open_connection_2(server, SOpenConnectionRequest2::read(payload)?)
                    .await;
            }
            _ => {
                log::error!("Bedrock: Received Unknown RakNet Offline packet: {packet_id}");
            }
        }
        Ok(())
    }

    pub async fn await_close_interrupt(&self) {
        self.close_interrupt.notified().await;
    }

    pub async fn get_packet_payload(&self, packet: Cursor<Vec<u8>>) -> Option<Bytes> {
        let mut network_reader = self.network_reader.lock().await;
        tokio::select! {
            () = self.await_close_interrupt() => {
                log::debug!("Canceling player packet processing");
                None
            },
            packet_result = network_reader.get_packet_payload(packet) => {
                match packet_result {
                    Ok(packet) => Some(packet),
                    Err(err) => {
                        if !matches!(err, PacketDecodeError::ConnectionClosed) {
                            log::warn!("Failed to decode packet from client: {err}");
                            let _text = format!("Error while reading incoming packet {err}");
                            self.close();
                            //self.kick(client, TextComponent::text(text)).await;
                        }
                        None
                    }
                }
            }
        }
    }

    pub fn spawn_task<F>(&self, task: F) -> Option<JoinHandle<F::Output>>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        if self.closed.load(Ordering::Relaxed) {
            None
        } else {
            Some(self.tasks.spawn(task))
        }
    }
}
