use base64::Engine;
use std::io::Cursor;
use std::sync::LazyLock;

use rustls_pki_types::CertificateDer;

const ADDITIONAL_CA_BUNDLE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/additional-ca.pem"));
static ROOTS: LazyLock<Vec<CertificateDer<'static>>> = LazyLock::new(|| {
    let mut roots = webpki_root_certs::TLS_SERVER_ROOT_CERTS.to_vec();
    if !ADDITIONAL_CA_BUNDLE.is_empty() {
        let additional = rustls_pemfile::certs(&mut Cursor::new(ADDITIONAL_CA_BUNDLE))
            .collect::<Result<Vec<_>, _>>()
            .expect("edition CA bundle is valid PEM");
        assert!(
            !additional.is_empty(),
            "non-empty edition CA bundle must contain a certificate"
        );
        roots.extend(additional);
    }
    roots
});

static PEM_BUNDLE: LazyLock<Vec<u8>> = LazyLock::new(|| {
    let mut pem = Vec::new();

    for certificate in ROOTS.iter() {
        pem.extend_from_slice(b"-----BEGIN CERTIFICATE-----\n");

        let encoded = base64::engine::general_purpose::STANDARD.encode(certificate.as_ref());
        for line in encoded.as_bytes().chunks(64) {
            pem.extend_from_slice(line);
            pem.push(b'\n');
        }

        pem.extend_from_slice(b"-----END CERTIFICATE-----\n");
    }

    pem
});

/// Mozilla WebPKI roots plus any CAs supplied by the selected AXE edition.
pub fn certificates() -> &'static [CertificateDer<'static>] {
    &ROOTS
}

/// The same roots encoded as a PEM CA bundle for libcurl-compatible consumers.
pub fn pem_bundle() -> &'static [u8] {
    &PEM_BUNDLE
}
