use base64::prelude::*;
use regex::Regex;
use std::sync::mpsc::{self};
use std::{sync::mpsc::Sender, thread::JoinHandle};
use which::which;

use super::{Forwarder, ThreadLoopForwarder, ThreadParameters};
use crate::network::netns::NetworkNamespace;

use crate::config::providers::OpenVpnProvider;
use crate::config::providers::pia::PrivateInternetAccess;
use crate::config::vpn::Protocol;

/// Used to provide port forwarding for PrivateInternetAccess
pub struct Piapf {
    pub port: u16,
    loop_thread_handle: Option<JoinHandle<()>>,
    send_channel: Sender<bool>,
}

pub struct ThreadParamsImpl {
    pub port: u16,
    pub netns_name: String,
    pub signature: String,
    pub payload: String,
    pub hostname: String,
    pub gateway: String,
    pub pia_cert_path: String,
    pub callback: Option<String>,
}

impl ThreadParameters for ThreadParamsImpl {
    fn get_callback_command(&self) -> Option<String> {
        self.callback.clone()
    }

    fn get_loop_delay(&self) -> u64 {
        60 * 15
    }

    fn get_netns_name(&self) -> String {
        self.netns_name.clone()
    }
}

impl Piapf {
    pub fn new(
        ns: &NetworkNamespace,
        config_file: &String,
        protocol: &Protocol,
        callback: Option<&String>,
    ) -> anyhow::Result<Self> {
        let pia = PrivateInternetAccess {};

        if which("traceroute").is_err() {
            log::error!(
                "The traceroute utility is necessary for PIA port forwarding. Please install traceroute."
            );
            anyhow::bail!(
                "The traceroute utility is necessary for PIA port forwarding. Please install traceroute."
            )
        }

        let traceroute_response = NetworkNamespace::exec_with_output(
            &ns.name,
            &["traceroute", "-n", "-m", "1", "privateinternetaccess.com"],
        )?;
        if !traceroute_response.status.success() {
            log::error!("Could not locate gateway with traceroute");
            anyhow::bail!("Could not locate gateway with traceroute")
        }
        let re = Regex::new(r" *1 *(?P<gateway>\d{1,3}.\d{1,3}.\d{1,3}.\d{1,3}).*")
            .expect("Unable to compile regex");
        let result = String::from_utf8_lossy(&traceroute_response.stdout);
        let second_line = result
            .lines()
            .nth(1)
            .expect("Missing second line (first hop) in traceroute");
        let vpn_gateway = re
            .captures(second_line)
            .expect("No captures from traceroute output")
            .get(1)
            .expect("No matching IP group in traceroute")
            .as_str()
            .to_string();

        log::info!("PIA gateway: {}", vpn_gateway);

        let vpn_hostname = match protocol {
            Protocol::OpenVpn => pia.hostname_for_openvpn_conf(config_file)?,
            Protocol::Wireguard => pia.hostname_for_wireguard_conf(config_file)?,
            _ => {
                log::error!("PIA port forwarding only supported for OpenVPN and Wireguard");
                anyhow::bail!("PIA port forwarding only supported for OpenVPN and Wireguard")
            }
        };

        log::info!("PIA hostname: {}", vpn_hostname);

        let (pia_user, pia_pass) = match protocol {
            Protocol::OpenVpn => pia.load_openvpn_auth()?,
            Protocol::Wireguard => pia.load_wireguard_auth()?,
            _ => {
                log::error!("PIA port forwarding only supported for OpenVPN and Wireguard");
                anyhow::bail!("PIA port forwarding only supported for OpenVPN and Wireguard")
            }
        };

        //log::info!("PIA u/p: {} / {}", pia_user, pia_pass);

        let pia_token = PrivateInternetAccess::get_pia_token(&pia_user, &pia_pass)?;
        let pia_cert_path = pia.pia_cert_path()?.display().to_string();

        log::info!("PIA pia_token: {}", pia_token);
        log::info!("PIA pia_cert_path: {}", pia_cert_path);

        if which("curl").is_err() {
            log::error!(
                "The curl utility is necessary for PIA port forwarding. Please install curl."
            );
            anyhow::bail!(
                "The curl utility is necessary for PIA port forwarding. Please install curl."
            )
        }

        let get_response = NetworkNamespace::exec_with_output(
            &ns.name,
            &[
                "curl",
                "-s",
                "-m",
                "5",
                "--connect-to",
                &format!("{}::{}:", vpn_hostname, vpn_gateway).to_string(),
                "--cacert",
                &pia_cert_path,
                "-G",
                "--data-urlencode",
                &format!("token={}", pia_token).to_string(),
                &format!("https://{}:19999/getSignature", vpn_hostname).to_string(),
            ],
        )?;
        if !get_response.status.success() {
            log::error!("Could not obtain signature for port forward from PIA API");
            anyhow::bail!("Could not obtain signature for port forward from PIA API")
        }

        let parsed = json::parse(String::from_utf8_lossy(&get_response.stdout).as_ref())?;
        if parsed["status"] != "OK" {
            log::error!("Signature for port forward from PIA API not OK");
            anyhow::bail!("Signature for port forward from PIA API not OK");
        }

        let signature = parsed["signature"]
            .as_str()
            .expect("getSignature response missing signature")
            .to_string();
        let payload = parsed["payload"]
            .as_str()
            .expect("getSignature response missing payload")
            .to_string();
        let decoded = BASE64_STANDARD.decode(&payload)?;
        let parsed = json::parse(String::from_utf8_lossy(&decoded).as_ref())?;
        let port = parsed["port"]
            .as_u16()
            .expect("getSignature response missing port");

        let params = ThreadParamsImpl {
            netns_name: ns.name.clone(),
            hostname: vpn_hostname,
            gateway: vpn_gateway,
            pia_cert_path,
            signature,
            payload,
            port,
            callback: callback.cloned(),
        };
        let port = Self::refresh_port(&params)?;
        Self::callback_command(&params, port);
        let (send, recv) = mpsc::channel::<bool>();
        let handle = std::thread::spawn(move || Self::thread_loop(params, recv));

        log::info!("PIA forwarded local port: {port}");
        Ok(Self {
            port,
            loop_thread_handle: Some(handle),
            send_channel: send,
        })
    }
}

impl ThreadLoopForwarder for Piapf {
    type ThreadParams = ThreadParamsImpl;

    fn refresh_port(params: &Self::ThreadParams) -> anyhow::Result<u16> {
        let bind_response = NetworkNamespace::exec_with_output(
            &params.netns_name,
            &[
                "curl",
                "-Gs",
                "-m",
                "5",
                "--connect-to",
                &format!("{}::{}:", params.hostname, params.gateway).to_string(),
                "--cacert",
                &params.pia_cert_path,
                "--data-urlencode",
                &format!("payload={}", params.payload).to_string(),
                "--data-urlencode",
                &format!("signature={}", params.signature).to_string(),
                &format!("https://{}:19999/bindPort", params.hostname).to_string(),
            ],
        )?;
        if !bind_response.status.success() {
            log::error!("Could not bind port forward from PIA API");
            anyhow::bail!("Could not bind port forward from PIA API")
        }

        let parsed = json::parse(String::from_utf8_lossy(&bind_response.stdout).as_ref())?;

        if parsed["status"] != "OK" {
            log::error!("Bind for port forward from PIA API not OK");
            anyhow::bail!("Bind for port forward from PIA API not OK");
        }

        if let Some(cb) = &params.callback {
            let refresh_response = NetworkNamespace::exec_with_output(
                &params.netns_name,
                &[cb, &params.port.to_string()],
            )?;
            if !refresh_response.status.success() {
                log::error!(
                    "Port forwarding callback script was unsuccessful!: stdout: {:?}, stderr: {:?}, exit code: {}",
                    String::from_utf8(refresh_response.stdout),
                    String::from_utf8(refresh_response.stderr),
                    refresh_response.status
                );
            } else if let Ok(out) = String::from_utf8(refresh_response.stdout) {
                println!("{}", out);
            }
        }

        log::info!("Successfully updated claim to port {}", params.port);

        Ok(params.port)
    }
}

impl Drop for Piapf {
    fn drop(&mut self) {
        let handle = self.loop_thread_handle.take();
        if let Some(h) = handle {
            self.send_channel.send(true).ok();
            h.join().ok();
        }
    }
}

impl Forwarder for Piapf {
    fn forwarded_port(&self) -> u16 {
        self.port
    }
}
