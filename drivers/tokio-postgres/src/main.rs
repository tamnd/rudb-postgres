//! Connects with tokio-postgres over rustls and prints the result of `SELECT 1`. tokio-postgres
//! sends a query through the extended flow.

use std::env;
use std::sync::Arc;

use rustls::RootCertStore;
use rustls_pki_types::CertificateDer;
use rustls_pki_types::pem::PemObject;
use tokio_postgres::config::SslMode;

fn var(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| panic!("{name} is not set"))
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut roots = RootCertStore::empty();
    for cert in CertificateDer::pem_file_iter(var("GATE_ROOT_CERT")).expect("the root certificate") {
        roots.add(cert.expect("a certificate")).expect("a root");
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("the protocol versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
    let mut config = tokio_postgres::Config::new();
    config
        .host(var("GATE_HOST"))
        .port(var("GATE_PORT").parse().expect("a port"))
        .user(var("GATE_USER"))
        .password(var("GATE_PASSWORD"))
        .dbname(var("GATE_DATABASE"))
        .ssl_mode(SslMode::Require);
    let connector = tokio_postgres_rustls::MakeRustlsConnect::new(tls);
    let (client, connection) = match config.connect(connector).await {
        Ok(pair) => pair,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };
    tokio::spawn(async move {
        if let Err(error) = connection.await {
            eprintln!("{error}");
        }
    });
    let row = client.query_one("select 1", &[]).await.expect("select 1");
    let one: i32 = row.get(0);
    println!("{one}");
}
