// SPDX-License-Identifier: Apache-2.0
//! Read-only discovery. No daemon configuration or credentials are accessed.
use crate::{Result, config::Config, server::Addresses};
use serde::Deserialize;
use std::{
    net::{IpAddr, SocketAddr},
    time::Duration,
};
use tokio::{net::TcpListener, sync::mpsc};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Default)]
pub struct Discovery {
    pub tailscale_ips: Vec<IpAddr>,
    pub dns_name: Option<String>,
    pub hostname: String,
}
impl Discovery {
    pub async fn detect() -> Self {
        let hostname = hostname::get()
            .ok()
            .and_then(|name| name.into_string().ok())
            .unwrap_or_else(|| "localhost".into());
        let mut result = Self {
            hostname,
            ..Self::default()
        };
        let mut command = tokio::process::Command::new("tailscale");
        command.args(["status", "--json"]).kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: Øphio may run without a console.
        if let Ok(Ok(output)) = tokio::time::timeout(Duration::from_secs(3), command.output()).await
            && output.status.success()
            && output.stdout.len() <= 4 * 1024 * 1024
            && let Ok(status) = serde_json::from_slice::<TailStatus>(&output.stdout)
            && status.backend_state == "Running"
        {
            result.tailscale_ips = status.this_host.ips;
            result.dns_name = status
                .this_host
                .dns_name
                .map(|name| name.trim_end_matches('.').to_owned())
                .filter(|name| !name.is_empty());
        }
        result
    }
    pub fn certificate_names(&self, config: &crate::config::Config) -> Vec<String> {
        let mut names = vec![self.hostname.clone()];
        names.extend(self.dns_name.clone());
        names.extend(self.tailscale_ips.iter().map(ToString::to_string));
        names.extend(
            config
                .listen
                .iter()
                .filter(|ip| !ip.is_unspecified())
                .map(ToString::to_string),
        );
        names
    }
    pub fn listen_addresses(
        &self,
        config: &crate::config::Config,
    ) -> Result<Vec<std::net::SocketAddr>> {
        config.addresses(&self.tailscale_ips)
    }
    pub fn offer_addresses(&self, config: &crate::config::Config) -> Result<Vec<String>> {
        let mut addresses = Vec::new();
        if let Some(name) = &self.dns_name {
            addresses.push(format!("{name}:{}", config.port));
        }
        for ip in &self.tailscale_ips {
            addresses.push(std::net::SocketAddr::new(*ip, config.port).to_string());
        }
        addresses.extend(
            self.listen_addresses(config)?
                .into_iter()
                .filter(|a| !a.ip().is_loopback() && !a.ip().is_unspecified())
                .map(|a| a.to_string()),
        );
        if addresses.is_empty() {
            addresses.push(format!("127.0.0.1:{}", config.port));
        }
        let mut seen = std::collections::HashSet::new();
        addresses.retain(|a| seen.insert(a.clone()));
        Ok(addresses)
    }
}

/// Tailscale can come up after Øphio does, as it may when the computer
/// starts: Øphio then listened on this computer alone, and no device could
/// reach it until it was restarted. Keeps looking, often at first and then
/// once a minute, and listens on each Tailscale address as it appears.
/// `listening` is what Øphio already listens at; listeners for the new
/// addresses go to `later`, and the pairing offer follows what is found.
pub async fn follow<D, F>(
    config: &Config,
    mut detect: D,
    mut listening: Vec<SocketAddr>,
    later: mpsc::UnboundedSender<TcpListener>,
    offered: Addresses,
    shutdown: CancellationToken,
) where
    D: FnMut() -> F,
    F: Future<Output = Discovery>,
{
    let mut wait = Duration::from_secs(2);
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return,
            () = tokio::time::sleep(wait) => {}
        }
        wait = (wait * 2).min(Duration::from_secs(60));
        let discovery = detect().await;
        if discovery.tailscale_ips.is_empty() {
            continue;
        }
        let Ok(addresses) = discovery.listen_addresses(config) else {
            continue;
        };
        let mut more = false;
        for address in addresses {
            // A wildcard of its family already covers it.
            if listening.iter().any(|known| {
                *known == address
                    || known.ip().is_unspecified()
                        && known.is_ipv4() == address.is_ipv4()
                        && known.port() == address.port()
            }) {
                continue;
            }
            match TcpListener::bind(address).await {
                Ok(listener) => {
                    if later.send(listener).is_err() {
                        return;
                    }
                    listening.push(address);
                    more = true;
                }
                Err(error) => {
                    tracing::warn!(%address, %error, "cannot listen at a Tailscale address yet");
                }
            }
        }
        if more {
            tracing::info!("{} listening on Tailscale", ports::PRODUCT_NAME);
        }
        if let Ok(addresses) = discovery.offer_addresses(config) {
            offered.set(addresses);
        }
    }
}

#[derive(Deserialize)]
struct TailStatus {
    #[serde(rename = "BackendState", default)]
    backend_state: String,
    #[serde(rename = "Self")]
    this_host: TailSelf,
}
#[derive(Deserialize)]
struct TailSelf {
    #[serde(rename = "TailscaleIPs", default)]
    ips: Vec<IpAddr>,
    #[serde(rename = "DNSName")]
    dns_name: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[tokio::test(start_paused = true)]
    async fn tailscale_coming_up_late_is_listened_on() {
        let config = Config {
            port: 0,
            ..Config::default()
        };
        // Tailscale is still starting at the first look, as at boot, and
        // running at the second.
        let looks = Arc::new(Mutex::new(0));
        let counted = looks.clone();
        let detect = move || {
            let look = {
                let mut looks = counted.lock().unwrap();
                *looks += 1;
                *looks
            };
            async move {
                Discovery {
                    tailscale_ips: if look > 1 {
                        vec!["127.0.0.2".parse().unwrap()]
                    } else {
                        vec![]
                    },
                    dns_name: Some("computer.example.ts.net".into()),
                    hostname: "computer".into(),
                }
            }
        };
        let listening = config.addresses(&[]).unwrap();
        let (later, mut listeners) = mpsc::unbounded_channel();
        let offered = Addresses::from(vec!["127.0.0.1:0".to_owned()]);
        let shutdown = CancellationToken::new();
        let following = tokio::spawn({
            let (config, offered, shutdown) = (config.clone(), offered.clone(), shutdown.clone());
            async move { follow(&config, detect, listening, later, offered, shutdown).await }
        });
        let listener = listeners.recv().await.unwrap();
        assert_eq!(
            listener.local_addr().unwrap().ip(),
            "127.0.0.2".parse::<IpAddr>().unwrap()
        );
        assert_eq!(offered.get(), ["computer.example.ts.net:0", "127.0.0.2:0"]);
        // Found once, it is not listened on twice.
        tokio::time::sleep(Duration::from_secs(300)).await;
        assert!(*looks.lock().unwrap() > 3);
        assert!(listeners.try_recv().is_err());
        shutdown.cancel();
        following.await.unwrap();
    }
}
