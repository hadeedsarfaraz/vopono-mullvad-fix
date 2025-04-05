use base64::{
    Engine as _,
    engine::{GeneralPurpose, general_purpose},
};

use serde::Deserialize;
use std::fmt::Display;
use x25519_dalek::{PublicKey, StaticSecret};

use rand_core::OsRng as X25519OsRng;

const B64_ENGINE: GeneralPurpose = general_purpose::STANDARD;

#[derive(Deserialize, Clone)]
pub struct WgKey {
    pub public: String,
    pub private: String,
}

impl std::fmt::Debug for WgKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WgKey")
            .field("public", &self.public)
            .field("private", &"********".to_string())
            .finish()
    }
}

#[allow(dead_code)]
#[derive(Deserialize, Debug, Clone)]
pub struct WgPeer {
    pub key: WgKey,
    pub ipv4_address: ipnet::Ipv4Net,
    pub ipv6_address: ipnet::Ipv6Net,
    ports: Vec<u16>,
    can_add_ports: bool,
}

impl Display for WgPeer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.key.public)
    }
}

pub fn generate_keypair() -> anyhow::Result<WgKey> {
    // Generate new keypair
    let private = StaticSecret::random_from_rng(X25519OsRng);
    let public = PublicKey::from(&private);
    let public_key = B64_ENGINE.encode(public.as_bytes());
    let private_key = B64_ENGINE.encode(private.to_bytes());

    let keypair = WgKey {
        public: public_key,
        private: private_key,
    };
    Ok(keypair)
}

pub fn generate_public_key(private_key: &str) -> anyhow::Result<String> {
    let private_bytes = B64_ENGINE.decode(private_key)?;
    let mut byte_array = [0; 32];
    byte_array.copy_from_slice(&private_bytes);

    let private = StaticSecret::from(byte_array);
    let public = PublicKey::from(&private);
    let public_key = B64_ENGINE.encode(public.as_bytes());
    Ok(public_key)
}
