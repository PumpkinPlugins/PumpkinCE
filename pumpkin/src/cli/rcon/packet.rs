use bytes::{BufMut, BytesMut};
use std::io::{BufRead, Cursor, Write};
use thiserror::Error;
use tokio::io;
use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum PacketToServer {
    Auth = 3,
    Command = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum KnownPacketType {
    Auth = 2,
    Response = 0,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketTypeFromServer {
    Known(KnownPacketType),
    Unknown(i32),
}

impl From<i32> for PacketTypeFromServer {
    fn from(value: i32) -> Self {
        match value {
            2 => PacketTypeFromServer::Known(KnownPacketType::Auth),
            0 => PacketTypeFromServer::Known(KnownPacketType::Response),
            _ => PacketTypeFromServer::Unknown(value),
        }
    }
}

#[derive(Error, Debug)]
pub enum PacketError {
    #[error("I/O error")]
    Io(#[from] io::Error),
    #[error("Invalid packet length")]
    InvalidLength,
    #[error("Invalid UTF-8 in body")]
    InvalidBody(#[from] std::string::FromUtf8Error),
}

#[derive(Debug)]
pub struct Packet {
    pub p_type: i32,
    pub body: String,
    pub id: i32,
}

impl Packet {
    pub fn new(p_type: i32, body: String, id: i32) -> Packet {
        Self { p_type, body, id }
    }

    pub async fn deserialize(incoming: &mut Vec<u8>) -> Result<Option<Self>, PacketError> {
        if incoming.len() < 4 {
            return Ok(None);
        }
        let mut cursor = Cursor::new(&incoming[..]);

        let length = cursor.read_i32_le().await? as usize;
        if !(10..=4096).contains(&length) {
            return Err(PacketError::InvalidLength);
        }

        // why this needed
        if length > incoming.len() {
            return Ok(None);
        }

        let id = cursor.read_i32_le().await?;
        let p_type = cursor.read_i32_le().await?;

        let mut body_bytes = vec![];
        cursor.read_until(0, &mut body_bytes)?;
        body_bytes.pop();

        cursor.read_u8().await?;

        if cursor.position() != (length + 4) as u64 {
            return Err(PacketError::InvalidLength);
        }

        incoming.drain(..length + 4);

        Ok(Some(Packet {
            id,
            p_type,
            body: String::from_utf8(body_bytes)?,
        }))
    }

    pub fn write_to_buf(&self) -> BytesMut {
        let length = 10 + self.body.len();
        let mut buf = BytesMut::with_capacity(length + 4);
        buf.put_i32_le(length as i32);
        buf.put_i32_le(self.id);
        buf.put_i32_le(self.p_type);
        buf.put_slice(self.body.as_bytes());
        buf.put_u8(0);
        buf.put_u8(0);
        buf
    }

    #[allow(dead_code)]
    pub fn write_to_sync<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        writer.write_all(&self.write_to_buf())
    }

    pub async fn write_to_async<W: AsyncWrite + Unpin>(&self, writer: &mut W) -> io::Result<()> {
        writer.write_all(&self.write_to_buf()).await
    }
}
