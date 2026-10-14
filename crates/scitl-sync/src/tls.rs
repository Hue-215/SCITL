//! 同期の接続のTLSの設定(`docs/sync/index.md` 3節)。TLS 1.3だけを使い、待ち受ける側は待ち受けの
//! たびに作る使い捨ての鍵の公開鍵をそのまま見せる(RFC 7250。証明書を作らない)。つなぐ側は
//! 公開鍵を信頼の根に照らさず、ハンドシェイクの署名がその鍵で正しいことだけを確かめる。相手が
//! 誰かは、このあと数字で確かめる(`pake`)。

use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls13_signature_with_raw_key, CryptoProvider};
use rustls::pki_types::{
    CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, SubjectPublicKeyInfoDer,
    UnixTime,
};
use rustls::server::{AlwaysResolvesServerRawPublicKeys, NoServerSessionStorage};
use rustls::sign::CertifiedKey;
use rustls::{ClientConfig, DigitallySignedStruct, ServerConfig, SignatureScheme};

use crate::SyncError;

/// ALPNで名乗るプロトコル名。別のプロトコルの相手と取り違えない。
pub(crate) const ALPN: &[u8] = b"scitl-sync/1";

/// TLS 1.3のexporterのラベル(RFC 8446 7.5)。数字で鍵を合わせるときに混ぜる値を取る。
pub(crate) const EXPORTER_LABEL: &[u8] = b"EXPORTER-scitl-sync-pake";

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::aws_lc_rs::default_provider())
}

/// 待ち受けのたびに作る。鍵はこの設定と一緒に捨てる。
pub(crate) fn server_config() -> Result<Arc<ServerConfig>, SyncError> {
    let provider = provider();
    let pkcs8 = aws_lc_rs::signature::Ed25519KeyPair::generate()
        .and_then(|key| key.to_pkcs8())
        .map_err(|_| rustls::Error::General("failed to generate a key".to_string()))?;
    let key = provider
        .key_provider
        .load_private_key(PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
            pkcs8.as_ref().to_vec(),
        )))?;
    let spki = key
        .public_key()
        .ok_or_else(|| rustls::Error::General("no public key".to_string()))?;
    let certified = CertifiedKey::new(vec![CertificateDer::from(spki.as_ref().to_vec())], key);

    let mut config = ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(AlwaysResolvesServerRawPublicKeys::new(Arc::new(
            certified,
        ))));
    config.alpn_protocols = vec![ALPN.to_vec()];
    config.session_storage = Arc::new(NoServerSessionStorage {});
    config.send_tls13_tickets = 0;
    Ok(Arc::new(config))
}

pub(crate) fn client_config() -> Result<Arc<ClientConfig>, SyncError> {
    let provider = provider();
    let verifier = AnyRawPublicKey {
        provider: provider.clone(),
    };
    let mut config = ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    config.alpn_protocols = vec![ALPN.to_vec()];
    config.resumption = rustls::client::Resumption::disabled();
    config.enable_sni = false;
    Ok(Arc::new(config))
}

/// どの公開鍵も受け入れ、ハンドシェイクの署名だけを確かめる。相手の確かめは数字で行う。
#[derive(Debug)]
struct AnyRawPublicKey {
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for AnyRawPublicKey {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if !intermediates.is_empty() {
            return Err(rustls::Error::General(
                "unexpected certificate chain".to_string(),
            ));
        }
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General("TLS 1.2 is not used".to_string()))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature_with_raw_key(
            message,
            &SubjectPublicKeyInfoDer::from(cert.as_ref()),
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }

    fn requires_raw_public_keys(&self) -> bool {
        true
    }
}
