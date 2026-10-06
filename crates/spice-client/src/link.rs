//! Link handshake and ticket authentication: `spice_channel_send_link`,
//! `recv_link_hdr`, `recv_link_msg`, `send_spice_ticket` and `recv_auth` in
//! spice-gtk `spice-channel.c`.

use bytes::BytesMut;
use log::{debug, log_enabled, trace};
use rsa::pkcs8::DecodePublicKey;
use rsa::{Oaep, RsaPublicKey};
use spice_proto::caps::{self, Caps};
use spice_proto::enums::{
    SPICE_MAGIC, SPICE_MAX_PASSWORD_LENGTH, SPICE_VERSION_MAJOR, SPICE_VERSION_MINOR, link_err,
};
use spice_proto::header::{LinkHeader, LinkMess, LinkReply, encode_auth_mechanism};
use spice_proto::{ProtoError, Reader};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::error::{ClientError, Result};

/// The largest link reply we accept: the fixed part plus generous caps.
const MAX_LINK_REPLY: u32 = 64 * 1024;

/// What the handshake negotiated.
#[derive(Debug, Clone)]
pub struct Linked {
    pub use_mini_header: bool,
    pub peer_minor: u32,
    pub common_caps: Caps,
    pub channel_caps: Caps,
}

/// Outcome of a link attempt that the caller may want to act on.
#[derive(Debug)]
pub enum LinkOutcome {
    Ok(Linked),
    /// `SPICE_LINK_ERR_NEED_SECURED`: reconnect on the TLS port.
    NeedSecured,
}

/// The common caps every spice-gtk channel advertises (minus SASL).
pub fn client_common_caps() -> Caps {
    Caps::new()
        .with(caps::common::PROTOCOL_AUTH_SELECTION)
        .with(caps::common::MINI_HEADER)
}

pub async fn link<S: AsyncRead + AsyncWrite + Unpin>(
    s: &mut S,
    name: &str,
    connection_id: u32,
    channel_type: u8,
    channel_id: u8,
    channel_caps: &Caps,
    password: &str,
) -> Result<LinkOutcome> {
    let mess = LinkMess {
        connection_id,
        channel_type,
        channel_id,
        common_caps: client_common_caps(),
        channel_caps: channel_caps.clone(),
    };
    let mut out = BytesMut::new();
    mess.encode_with_header(SPICE_VERSION_MAJOR, SPICE_VERSION_MINOR, &mut out);
    debug!(
        "{name}: link: connection id {connection_id}, {} common / {} channel cap words",
        mess.common_caps.words().len(),
        mess.channel_caps.words().len()
    );
    s.write_all(&out).await?;
    s.flush().await?;

    // recv_link_hdr
    let mut hdr = [0u8; LinkHeader::SIZE];
    s.read_exact(&mut hdr).await?;
    let hdr = LinkHeader::decode(&mut Reader::new(&hdr))?;
    if hdr.magic != SPICE_MAGIC {
        return Err(ProtoError::Other("invalid SPICE magic in link reply".into()).into());
    }
    debug!(
        "{name}: peer version {}.{}",
        hdr.major_version, hdr.minor_version
    );
    if hdr.major_version != SPICE_VERSION_MAJOR {
        return Err(ProtoError::Other(format!(
            "major version mismatch (got {}, expected {SPICE_VERSION_MAJOR})",
            hdr.major_version
        ))
        .into());
    }
    if hdr.size < LinkReply::SIZE as u32 || hdr.size > MAX_LINK_REPLY {
        return Err(ProtoError::Other(format!("invalid link reply size {}", hdr.size)).into());
    }

    // recv_link_msg
    let mut body = vec![0u8; hdr.size as usize];
    s.read_exact(&mut body).await?;
    let reply = LinkReply::decode(&body)?;
    match reply.error {
        link_err::OK => {}
        link_err::NEED_SECURED => {
            debug!("{name}: server requires TLS for this channel");
            return Ok(LinkOutcome::NeedSecured);
        }
        e => return Err(ClientError::Link(e)),
    }
    if log_enabled!(log::Level::Trace) {
        trace!(
            "{name}: remote common caps {:?}, channel caps {:?}",
            reply.common_caps.iter().collect::<Vec<_>>(),
            reply.channel_caps.iter().collect::<Vec<_>>()
        );
    }

    if !reply
        .common_caps
        .test(caps::common::PROTOCOL_AUTH_SELECTION)
    {
        debug!("{name}: server supports spice ticket auth only");
    } else if reply.common_caps.test(caps::common::AUTH_SPICE) {
        let mut b = BytesMut::new();
        encode_auth_mechanism(caps::common::AUTH_SPICE, &mut b);
        s.write_all(&b).await?;
    } else {
        return Err(ClientError::Unsupported(
            "no compatible authentication mechanism (server wants SASL)".into(),
        ));
    }
    let ticket = encrypt_ticket(&reply.pub_key, password)?;
    s.write_all(&ticket).await?;
    s.flush().await?;

    // recv_auth
    let result = s.read_u32_le().await?;
    if result != link_err::OK {
        debug!("{name}: link result: reply {result}");
        return Err(if result == link_err::PERMISSION_DENIED {
            ClientError::AuthFailed
        } else {
            ClientError::Link(result)
        });
    }

    let use_mini_header = reply.common_caps.test(caps::common::MINI_HEADER);
    debug!("{name}: use mini header: {use_mini_header}");
    Ok(LinkOutcome::Ok(Linked {
        use_mini_header,
        peer_minor: hdr.minor_version,
        common_caps: reply.common_caps,
        channel_caps: reply.channel_caps,
    }))
}

/// RSA-OAEP(SHA-1) encrypt the password, including its NUL terminator, with
/// the server's ticket key (`spice_channel_send_spice_ticket`).
pub fn encrypt_ticket(pub_key_der: &[u8], password: &str) -> Result<Vec<u8>> {
    if password.len() > SPICE_MAX_PASSWORD_LENGTH {
        return Err(ClientError::PasswordTooLong);
    }
    let key = RsaPublicKey::from_public_key_der(pub_key_der)
        .map_err(|e| ProtoError::Other(format!("bad ticket public key: {e}")))?;
    let mut plain = Vec::with_capacity(password.len() + 1);
    plain.extend_from_slice(password.as_bytes());
    plain.push(0);
    let enc = key
        .encrypt(&mut rand::thread_rng(), Oaep::new::<sha1::Sha1>(), &plain)
        .map_err(|e| ProtoError::Other(format!("ticket encryption failed: {e}")))?;
    plain.fill(0);
    Ok(enc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsa::pkcs8::EncodePublicKey;
    use rsa::{RsaPrivateKey, traits::PublicKeyParts};
    use spice_proto::enums::SPICE_TICKET_PUBKEY_BYTES;

    fn keypair() -> (RsaPrivateKey, Vec<u8>) {
        let k = RsaPrivateKey::new(&mut rand::thread_rng(), 1024).unwrap();
        let der = k.to_public_key().to_public_key_der().unwrap().into_vec();
        (k, der)
    }

    #[test]
    fn ticket_round_trip() {
        let (k, der) = keypair();
        assert_eq!(der.len(), SPICE_TICKET_PUBKEY_BYTES);
        let enc = encrypt_ticket(&der, "secret").unwrap();
        assert_eq!(enc.len(), k.size());
        let dec = k.decrypt(Oaep::new::<sha1::Sha1>(), &enc).unwrap();
        assert_eq!(dec, b"secret\0");
        assert!(matches!(
            encrypt_ticket(&der, &"x".repeat(61)),
            Err(ClientError::PasswordTooLong)
        ));
    }

    /// A scripted server end of the handshake.
    async fn fake_server(
        mut s: tokio::io::DuplexStream,
        der: Vec<u8>,
        k: RsaPrivateKey,
        error: u32,
        auth_result: u32,
    ) -> Vec<u8> {
        let mut hdr = [0u8; 16];
        s.read_exact(&mut hdr).await.unwrap();
        let h = LinkHeader::decode(&mut Reader::new(&hdr)).unwrap();
        let mut body = vec![0; h.size as usize];
        s.read_exact(&mut body).await.unwrap();
        let mess = LinkMess::decode(&body).unwrap();
        assert!(mess.common_caps.test(caps::common::MINI_HEADER));

        let reply = LinkReply {
            error,
            pub_key: der.try_into().unwrap(),
            common_caps: Caps::new()
                .with(caps::common::PROTOCOL_AUTH_SELECTION)
                .with(caps::common::AUTH_SPICE)
                .with(caps::common::MINI_HEADER),
            channel_caps: Caps::new(),
        };
        let mut b = BytesMut::new();
        reply.encode(&mut b);
        let mut out = BytesMut::new();
        LinkHeader {
            magic: SPICE_MAGIC,
            major_version: 2,
            minor_version: 2,
            size: b.len() as u32,
        }
        .encode(&mut out);
        out.extend_from_slice(&b);
        s.write_all(&out).await.unwrap();
        if error != link_err::OK {
            return vec![];
        }
        assert_eq!(s.read_u32_le().await.unwrap(), caps::common::AUTH_SPICE);
        let mut ticket = [0u8; 128];
        s.read_exact(&mut ticket).await.unwrap();
        s.write_u32_le(auth_result).await.unwrap();
        k.decrypt(Oaep::new::<sha1::Sha1>(), &ticket).unwrap()
    }

    #[tokio::test]
    async fn handshake() {
        let (k, der) = keypair();
        let (mut c, s) = tokio::io::duplex(4096);
        let srv = tokio::spawn(fake_server(s, der, k, link_err::OK, link_err::OK));
        let out = link(&mut c, "main-0", 0, 1, 0, &Caps::new(), "pw")
            .await
            .unwrap();
        let LinkOutcome::Ok(l) = out else { panic!() };
        assert!(l.use_mini_header);
        assert_eq!(srv.await.unwrap(), b"pw\0");
    }

    #[tokio::test]
    async fn handshake_wrong_password() {
        let (k, der) = keypair();
        let (mut c, s) = tokio::io::duplex(4096);
        tokio::spawn(fake_server(
            s,
            der,
            k,
            link_err::OK,
            link_err::PERMISSION_DENIED,
        ));
        let e = link(&mut c, "main-0", 0, 1, 0, &Caps::new(), "")
            .await
            .unwrap_err();
        assert!(e.is_auth_failure());
    }

    #[tokio::test]
    async fn handshake_need_secured() {
        let (k, der) = keypair();
        let (mut c, s) = tokio::io::duplex(4096);
        tokio::spawn(fake_server(s, der, k, link_err::NEED_SECURED, 0));
        let out = link(&mut c, "main-0", 0, 1, 0, &Caps::new(), "")
            .await
            .unwrap();
        assert!(matches!(out, LinkOutcome::NeedSecured));
    }
}
