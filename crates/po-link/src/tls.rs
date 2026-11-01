//! TLS without an authority: each device presents a self-signed certificate made from its
//! Ed25519 key, and each side accepts only the keys it knows — those listed in the account's
//! directory. The certificate is a wrapper; the key is what is checked.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{DigitallySignedStruct, DistinguishedName, SignatureScheme};

use crate::identity::{Identity, device_id};

/// What both ends say they speak.
pub const ALPN: &[u8] = b"po-link/1";
/// The name a phone asks for: certificates are not checked by name, but TLS wants one.
pub const SERVER_NAME: &str = "po-link";

/// The DER prefix of an Ed25519 public key (RFC 8410 SubjectPublicKeyInfo); the 32 bytes of the
/// key follow it.
const ED25519_SPKI: [u8; 12] = [0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00];

/// The Ed25519 key a certificate carries, if it carries exactly one.
pub fn certificate_key(der: &[u8]) -> Option<[u8; 32]> {
    let mut found = None;
    for i in 0..der.len().saturating_sub(ED25519_SPKI.len() + 32 - 1) {
        if der[i..].starts_with(&ED25519_SPKI) {
            if found.is_some() {
                return None;
            }
            found = der.get(i + 12..i + 44).and_then(|k| k.try_into().ok());
        }
    }
    found
}

/// A device known from the directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peer {
    pub id: String,
    pub name: String,
    pub ed: [u8; 32],
}

/// The keys accepted, changing as the directory does: a device listed a second ago connects, a
/// revoked one no longer can.
#[derive(Debug, Default)]
pub struct Trust {
    keys: RwLock<HashMap<[u8; 32], Peer>>,
}

impl Trust {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn set(&self, peers: impl IntoIterator<Item = Peer>) {
        let map = peers.into_iter().map(|p| (p.ed, p)).collect();
        *self.keys.write().unwrap_or_else(|e| e.into_inner()) = map;
    }

    pub fn add(&self, peer: Peer) {
        self.keys.write().unwrap_or_else(|e| e.into_inner()).insert(peer.ed, peer);
    }

    pub fn get(&self, ed: &[u8; 32]) -> Option<Peer> {
        self.keys.read().unwrap_or_else(|e| e.into_inner()).get(ed).cloned()
    }

    pub fn peers(&self) -> Vec<Peer> {
        self.keys.read().unwrap_or_else(|e| e.into_inner()).values().cloned().collect()
    }
}

/// The certificate and key a device presents.
pub fn credentials(identity: &Identity) -> (Vec<CertificateDer<'static>>, PrivateKeyDer<'static>) {
    let pkcs8 = identity.pkcs8();
    let key = rcgen::KeyPair::from_pkcs8_der_and_sign_algo(&PrivatePkcs8KeyDer::from(pkcs8.clone()), &rcgen::PKCS_ED25519).expect("an Ed25519 key");
    let mut params = rcgen::CertificateParams::new(vec![SERVER_NAME.to_owned()]).expect("a valid name");
    params.distinguished_name.push(rcgen::DnType::CommonName, format!("po-link {}", identity.id()));
    // Far in the past and the future: validity dates are not what is trusted, and a phone with a
    // wrong clock must still connect.
    params.not_before = rcgen::date_time_ymd(2000, 1, 1);
    params.not_after = rcgen::date_time_ymd(2999, 12, 31);
    let cert = params.self_signed(&key).expect("a certificate");
    (vec![cert.der().clone()], PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(pkcs8)))
}

pub(crate) fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn tls_error(text: &str) -> rustls::Error {
    rustls::Error::General(text.to_owned())
}

fn check13(message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
    // Only Ed25519: the keys are ours, nothing else is expected.
    if dss.scheme != SignatureScheme::ED25519 {
        return Err(tls_error("only Ed25519"));
    }
    rustls::crypto::verify_tls13_signature(message, cert, dss, &provider().signature_verification_algorithms)
}

/// The PC side: a client certificate is required, and its key must be in [`Trust`].
#[derive(Debug)]
struct ClientKeys(Arc<Trust>);

impl ClientCertVerifier for ClientKeys {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(&self, end_entity: &CertificateDer<'_>, _: &[CertificateDer<'_>], _: UnixTime) -> Result<ClientCertVerified, rustls::Error> {
        let key = certificate_key(end_entity).ok_or_else(|| tls_error("no Ed25519 key"))?;
        match self.0.get(&key) {
            Some(_) => Ok(ClientCertVerified::assertion()),
            None => Err(tls_error(&format!("unknown device {}", device_id(&key)))),
        }
    }

    fn verify_tls12_signature(&self, _: &[u8], _: &CertificateDer<'_>, _: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(tls_error("TLS 1.3 only"))
    }

    fn verify_tls13_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        check13(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ED25519]
    }
}

/// The phone side: the PC must present the key the directory gives for it.
#[derive(Debug)]
struct ServerKey([u8; 32]);

impl ServerCertVerifier for ServerKey {
    fn verify_server_cert(&self, end_entity: &CertificateDer<'_>, _: &[CertificateDer<'_>], _: &ServerName<'_>, _: &[u8], _: UnixTime) -> Result<ServerCertVerified, rustls::Error> {
        match certificate_key(end_entity) {
            Some(key) if key == self.0 => Ok(ServerCertVerified::assertion()),
            _ => Err(tls_error("not the expected PC")),
        }
    }

    fn verify_tls12_signature(&self, _: &[u8], _: &CertificateDer<'_>, _: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(tls_error("TLS 1.3 only"))
    }

    fn verify_tls13_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        check13(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ED25519]
    }
}

/// The transport settings of every link: a phone on a mobile network loses a silent UDP mapping
/// within half a minute, so the link speaks every few seconds.
fn transport() -> Arc<quinn::TransportConfig> {
    let mut transport = quinn::TransportConfig::default();
    transport.keep_alive_interval(Some(std::time::Duration::from_secs(5)));
    transport.max_idle_timeout(Some(quinn::VarInt::from_u32(30_000).into()));
    transport.max_concurrent_bidi_streams(64u32.into());
    Arc::new(transport)
}

/// The QUIC server side of a PC.
pub fn server_config(identity: &Identity, trust: Arc<Trust>) -> quinn::ServerConfig {
    let (chain, key) = credentials(identity);
    let mut tls = rustls::ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .expect("TLS 1.3")
        .with_client_cert_verifier(Arc::new(ClientKeys(trust)))
        .with_single_cert(chain, key)
        .expect("our own certificate");
    tls.alpn_protocols = vec![ALPN.to_vec()];
    // No 0-RTT: data sent before the handshake ends could be replayed by whoever records it.
    tls.max_early_data_size = 0;
    let crypto = quinn::crypto::rustls::QuicServerConfig::try_from(tls).expect("TLS 1.3 for QUIC");
    let mut config = quinn::ServerConfig::with_crypto(Arc::new(crypto));
    config.transport_config(transport());
    // A phone changing networks keeps its connection.
    config.migration(true);
    config
}

/// The QUIC client side of a phone, for the PC whose key is `pc`.
pub fn client_config(identity: &Identity, pc: [u8; 32]) -> quinn::ClientConfig {
    let (chain, key) = credentials(identity);
    let mut tls = rustls::ClientConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .expect("TLS 1.3")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(ServerKey(pc)))
        .with_client_auth_cert(chain, key)
        .expect("our own certificate");
    tls.alpn_protocols = vec![ALPN.to_vec()];
    tls.enable_early_data = false;
    let crypto = quinn::crypto::rustls::QuicClientConfig::try_from(tls).expect("TLS 1.3 for QUIC");
    let mut config = quinn::ClientConfig::new(Arc::new(crypto));
    config.transport_config(transport());
    config
}

/// The key of the device at the other end of an established connection.
pub fn peer_key(connection: &quinn::Connection) -> Option<[u8; 32]> {
    let identity = connection.peer_identity()?;
    let chain = identity.downcast::<Vec<CertificateDer<'static>>>().ok()?;
    certificate_key(chain.first()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_certificate_carries_the_key_it_was_made_from() {
        let identity = Identity::generate();
        let (chain, _) = credentials(&identity);
        assert_eq!(certificate_key(&chain[0]), Some(identity.public().ed));
        assert_eq!(certificate_key(b"garbage"), None);
        // Two keys in one certificate: ambiguous, refused.
        let mut double = chain[0].to_vec();
        double.extend_from_slice(&ED25519_SPKI);
        double.extend_from_slice(&[7u8; 32]);
        assert_eq!(certificate_key(&double), None);
    }

    #[test]
    fn the_trust_changes_with_the_directory() {
        let trust = Trust::new();
        let peer = Peer { id: "a".into(), name: "phone".into(), ed: [1; 32] };
        trust.set([peer.clone()]);
        assert_eq!(trust.get(&[1; 32]), Some(peer));
        trust.set([]);
        assert!(trust.get(&[1; 32]).is_none());
    }
}
