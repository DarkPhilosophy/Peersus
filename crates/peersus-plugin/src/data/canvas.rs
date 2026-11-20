//! The owner redirects an authenticated desktop canvas claim; it never reads a raster.
use crate::{link, transport};
use std::{io, io::Read, io::Write, path::Path, time::Duration};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const MAGIC: [u8; 4] = *b"PSC1";
pub const TOKEN_BYTES: usize = 32;
pub const ROUTE_WAIT: Duration = Duration::from_secs(1);
pub type Token = [u8; TOKEN_BYTES];

/// Separate from the worker's bootstrap token: this authorizes only one connection to one
/// mounted view. It is exchanged between trusted process bootstraps, not guest memory.
pub struct Redirect {
    endpoint: String,
    token: Token,
}

impl Redirect {
    pub fn new(endpoint: String, token: Token) -> Option<Self> {
        (!endpoint.is_empty()
            && endpoint.len() <= u8::MAX as usize
            && !endpoint.as_bytes().contains(&0)
            && transport::local_endpoint(Path::new(&endpoint)))
        .then_some(Self { endpoint, token })
    }

    pub async fn write(&self, stream: &mut (impl AsyncWrite + Unpin)) -> io::Result<()> {
        stream.write_all(&MAGIC).await?;
        stream.write_u8(self.endpoint.len() as u8).await?;
        stream.write_all(self.endpoint.as_bytes()).await?;
        stream.write_all(&self.token).await
    }

    /// After DataHello, obtain the destination from the owner and authenticate a new
    /// socket to the actual consumer. The owner connection carries no drawing data.
    pub fn follow(mut owner: transport::BlockingStream) -> io::Result<transport::BlockingStream> {
        let mut head = [0; 5];
        owner.read_exact(&mut head)?;
        if head[..4] != MAGIC || head[4] == 0 {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let len = usize::from(head[4]);
        let mut endpoint = [0; u8::MAX as usize];
        owner.read_exact(&mut endpoint[..len])?;
        let mut token = [0; TOKEN_BYTES];
        owner.read_exact(&mut token)?;
        drop(owner);
        let endpoint =
            std::str::from_utf8(&endpoint[..len]).map_err(|_| io::ErrorKind::InvalidData)?;
        let mut stream = transport::connect_blocking(endpoint)?;
        stream.set_write_timeout(Some(ROUTE_WAIT))?;
        stream.write_all(&MAGIC)?;
        stream.write_all(&token)?;
        Ok(stream)
    }
}

pub async fn authenticate(
    stream: &mut (impl AsyncRead + Unpin),
    expected: &Token,
) -> io::Result<()> {
    let mut wire = [0; MAGIC.len() + TOKEN_BYTES];
    stream.read_exact(&mut wire).await?;
    if wire[..MAGIC.len()] != MAGIC || !link::same_bytes(&wire[MAGIC.len()..], expected) {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok(())
}
