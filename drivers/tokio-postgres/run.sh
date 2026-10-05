# tokio-postgres over rustls. rustls checks the certificate and the host name, and SslMode::Require
# refuses a server without TLS.
set -e
cargo build --quiet --release --target-dir build
build/release/tokio-postgres-smoke
