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
    pub v4_public: Option<std::net::Ipv4Addr>,
    pub v6_prefix: std::net::Ipv6Addr,
}