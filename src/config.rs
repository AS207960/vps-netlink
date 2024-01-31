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

#[derive(serde::Deserialize)]
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

impl serde::ser::Serialize for V4Ip {
    fn serialize<S: serde::ser::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;

        let val = self.as_many();
        let mut seq = ser.serialize_seq(Some(val.len()))?;
        for ip in val {
            seq.serialize_element(ip)?;
        }
        seq.end()
    }
}