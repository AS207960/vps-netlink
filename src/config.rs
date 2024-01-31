#[derive(serde::Deserialize)]
pub struct Config {
    pub rt_proto: u8,
    pub interface: String,
    pub vps: Vec<VPS>
}

#[derive(serde::Deserialize, serde::Serialize)]
pub struct VPS {
    pub vlan: u16,
    pub v4_addr: std::net::Ipv4Addr,
    pub v4_public: Option<V4Ip>,
    pub v6_prefix: std::net::Ipv6Addr,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum V4Ip {
    One(std::net::Ipv4Addr),
    Many(Vec<std::net::Ipv4Addr>),
}

impl V4Ip {
    pub fn as_many(&self) -> &[std::net::Ipv4Addr] {
        match self {
            Self::One(ip) => std::slice::from_ref(ip),
            Self::Many(ip) => ip,
        }
    }
}