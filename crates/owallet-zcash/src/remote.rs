//! lightwalletd server selection + connection (direct TLS).
//!
//! Adapted from zkv's `remote.rs`, trimmed to direct connections (no SOCKS) —
//! owallet routes its own networking. Accepts either a built-in operator alias
//! (`zecrocks`/`ywallet`/`ecc`) or a comma-separated `host:port` list.

use std::borrow::Cow;

use tonic::transport::{Channel, ClientTlsConfig};
use zcash_client_backend::proto::service::compact_tx_streamer_client::CompactTxStreamerClient;
use zcash_protocol::consensus::Network as ConsensusNetwork;

use crate::{error::ZcashError, network::Network};

const ECC_TESTNET: &[Server<'_>] = &[Server::fixed("lightwalletd.testnet.electriccoin.co", 9067)];
const YWALLET_MAINNET: &[Server<'_>] = &[Server::fixed("lwd1.zcash-infra.com", 9067)];
const ZEC_ROCKS_MAINNET: &[Server<'_>] = &[Server::fixed("zec.rocks", 443)];
const ZEC_ROCKS_TESTNET: &[Server<'_>] = &[Server::fixed("testnet.zec.rocks", 443)];

#[derive(Clone, Debug)]
struct Server<'a> {
    host: Cow<'a, str>,
    port: u16,
}

impl Server<'static> {
    const fn fixed(host: &'static str, port: u16) -> Self {
        Self {
            host: Cow::Borrowed(host),
            port,
        }
    }
}

impl Server<'_> {
    fn use_tls(&self) -> bool {
        !matches!(self.host.as_ref(), "localhost" | "127.0.0.1" | "::1")
            && !self.host.ends_with(".onion")
    }

    fn endpoint(&self) -> String {
        format!(
            "{}://{}:{}",
            if self.use_tls() { "https" } else { "http" },
            self.host,
            self.port
        )
    }

    async fn connect(&self) -> Result<CompactTxStreamerClient<Channel>, ZcashError> {
        let channel = Channel::from_shared(self.endpoint()).map_err(ZcashError::transport)?;
        let channel = if self.use_tls() {
            channel
                .tls_config(
                    ClientTlsConfig::new()
                        .domain_name(self.host.to_string())
                        .assume_http2(true)
                        .with_webpki_roots(),
                )
                .map_err(ZcashError::transport)?
        } else {
            channel
        };
        Ok(CompactTxStreamerClient::new(
            channel.connect().await.map_err(ZcashError::transport)?,
        ))
    }
}

/// Resolve a server spec + network to a single lightwalletd server.
fn pick<'a>(spec: &'a str, network: Network) -> Result<Server<'a>, ZcashError> {
    let net: ConsensusNetwork = network.into();
    let hosted = |list: &[Server<'static>]| -> Result<Server<'static>, ZcashError> {
        list.first()
            .cloned()
            .ok_or_else(|| ZcashError::transport(format!("operator does not serve {net:?}")))
    };
    match spec.trim() {
        "zecrocks" | "" => match net {
            ConsensusNetwork::MainNetwork => hosted(ZEC_ROCKS_MAINNET),
            ConsensusNetwork::TestNetwork => hosted(ZEC_ROCKS_TESTNET),
        },
        "ywallet" => match net {
            ConsensusNetwork::MainNetwork => hosted(YWALLET_MAINNET),
            ConsensusNetwork::TestNetwork => {
                Err(ZcashError::transport("ywallet has no testnet server"))
            }
        },
        "ecc" => match net {
            ConsensusNetwork::TestNetwork => hosted(ECC_TESTNET),
            ConsensusNetwork::MainNetwork => {
                Err(ZcashError::transport("ecc has no mainnet server"))
            }
        },
        other => other
            .split(',')
            .next()
            .and_then(|sub| {
                sub.rsplit_once(':').and_then(|(host, port)| {
                    port.parse().ok().map(|port| Server {
                        host: Cow::Owned(host.to_string()),
                        port,
                    })
                })
            })
            .ok_or_else(|| {
                ZcashError::transport(format!(
                    "'{other}' must be 'zecrocks'/'ywallet'/'ecc' or a host:port"
                ))
            }),
    }
}

/// Connect to a lightwalletd server for `network`. `spec` is an operator alias
/// (`zecrocks` default) or a `host:port`.
pub(crate) async fn connect(
    spec: &str,
    network: Network,
) -> Result<CompactTxStreamerClient<Channel>, ZcashError> {
    pick(spec, network)?.connect().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_operator_and_hostport() {
        // Default mainnet server is zec.rocks:443.
        let main = pick("zecrocks", Network::Main).unwrap();
        assert_eq!(main.host, "zec.rocks");
        assert_eq!(main.port, 443);
        assert!(main.use_tls());
        // Empty spec also defaults to zecrocks.
        assert_eq!(pick("", Network::Main).unwrap().host, "zec.rocks");
        assert_eq!(pick("", Network::Test).unwrap().host, "testnet.zec.rocks");
        let custom = pick("localhost:9067", Network::Main).unwrap();
        assert_eq!(custom.host, "localhost");
        assert_eq!(custom.port, 9067);
        assert!(!custom.use_tls());
        assert!(pick("not-a-host", Network::Main).is_err());
    }
}
