use futures_util::TryStreamExt;
use crate::Error;

#[derive(Debug)]
pub struct Interface {
    pub name: String,
    pub index: u32,
    pub link: u32,
    pub vlan: u16,
}

#[derive(Debug)]
pub struct Address {
    pub interface: u32,
    pub address: std::net::IpAddr,
    pub prefix_length: u8,
    pub message: netlink_packet_route::address::AddressMessage,
}

#[derive(Debug)]
pub struct Route {
    pub destination: std::net::IpAddr,
    pub destination_prefix_length: u8,
    pub interface: u32,
    pub message: netlink_packet_route::route::RouteMessage,
}

#[derive(Debug)]
pub struct State {
    pub interfaces: Vec<Interface>,
    pub addresses: Vec<Address>,
    pub routes: Vec<Route>,
}

async fn get_vlan_interfaces(handle: &rtnetlink::Handle) -> Result<Vec<Interface>, Error> {
    let mut links = handle.link().get().execute();
    let mut interfaces = vec![];

    let mut vps_infs = vec![];

    'outer: while let Some(msg) = links.try_next().await? {
        for nla in &msg.nlas {
            if let netlink_packet_route::nlas::link::Nla::Info(infos) = nla {
                for info in infos {
                    if let netlink_packet_route::nlas::link::Info::Kind(
                        netlink_packet_route::nlas::link::InfoKind::Vlan
                    ) = info {
                        for nla in &msg.nlas {
                            if let netlink_packet_route::nlas::link::Nla::IfName(name) = nla {
                                if name.starts_with("vps") {
                                    vps_infs.push(msg);
                                    continue 'outer;
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    for msg in vps_infs {
        let mut inf = Interface {
            index: msg.header.index,
            name: String::default(),
            link: 0,
            vlan: 0
        };

        for nla in msg.nlas {
            match nla {
                netlink_packet_route::nlas::link::Nla::Info(infos) => {
                    for info in infos {
                        if let netlink_packet_route::nlas::link::Info::Data(
                            netlink_packet_route::nlas::link::InfoData::Vlan(data)
                        ) = info {
                            for datum in data {
                                if let netlink_packet_route::nlas::link::InfoVlan::Id(
                                    vlan
                                ) = datum {
                                    inf.vlan = vlan
                                }
                            }
                        }
                    }
                }
                netlink_packet_route::nlas::link::Nla::Link(link) => {
                    inf.link = link;
                },
                netlink_packet_route::nlas::link::Nla::IfName(name) => {
                    inf.name = name;
                },
                _ => {}
            }
        }

        interfaces.push(inf);
    }

    Ok(interfaces)
}


async fn get_addresses(handle: &rtnetlink::Handle) -> Result<Vec<Address>, Error> {
    let mut addresses = vec![];

    let mut res = handle.address().get().execute();
    while let Some(msg) = res.try_next().await? {
        if msg.header.scope != netlink_packet_route::constants::RT_SCOPE_UNIVERSE {
            continue;
        }

        let mut address = Address {
            interface: msg.header.index,
            address: std::net::IpAddr::from([0, 0, 0, 0]),
            prefix_length: msg.header.prefix_len,
            message: msg.clone()
        };


        for nla in msg.nlas {
            match nla {
                netlink_packet_route::nlas::address::Nla::Address(d) => {
                    match msg.header.family as u16 {
                        netlink_packet_route::constants::AF_INET => {
                            let data: [u8; 4] = d.try_into().unwrap();
                            address.address = std::net::IpAddr::V4(
                                std::net::Ipv4Addr::from(data)
                            )
                        }
                        netlink_packet_route::constants::AF_INET6 => {
                            let data: [u8; 16] = d.try_into().unwrap();
                            address.address = std::net::IpAddr::V6(
                                std::net::Ipv6Addr::from(data)
                            )
                        },
                        _ => {}
                    }
                },
                _ => {}
            }
        }

        addresses.push(address);
    }

    Ok(addresses)
}


async fn get_routes(handle: &rtnetlink::Handle, route_proto: u8) -> Result<Vec<Route>, Error> {
    let mut routes = vec![];
    let mut vps_routes = vec![];

    let mut v4_routes = handle.route().get(rtnetlink::IpVersion::V4).execute();
    while let Some(msg) = v4_routes.try_next().await? {
        if msg.header.protocol == route_proto {
            vps_routes.push(msg);
        }
    }

    let mut v6_routes = handle.route().get(rtnetlink::IpVersion::V6).execute();
    while let Some(msg) = v6_routes.try_next().await? {
        if msg.header.protocol == route_proto {
            vps_routes.push(msg);
        }
    }

    'outer: for msg in vps_routes {
        let mut route = Route {
            destination: std::net::IpAddr::from([0, 0, 0, 0]),
            destination_prefix_length: msg.header.destination_prefix_length,
            interface: 0,
            message: msg.clone(),
        };

        for nla in msg.nlas {
            match nla {
                netlink_packet_route::nlas::route::Nla::Oif(i) => {
                    route.interface = i;
                },
                netlink_packet_route::nlas::route::Nla::Destination(d) => {
                    match msg.header.address_family as u16 {
                        netlink_packet_route::constants::AF_INET => {
                            let data: [u8; 4] = d.try_into().unwrap();
                            route.destination = std::net::IpAddr::V4(
                                std::net::Ipv4Addr::from(data)
                            )
                        }
                        netlink_packet_route::constants::AF_INET6 => {
                            let data: [u8; 16] = d.try_into().unwrap();
                            route.destination = std::net::IpAddr::V6(
                                std::net::Ipv6Addr::from(data)
                            )
                        },
                        _ => continue 'outer
                    }
                }
                _ => {}
            }
        }

        routes.push(route);
    }

    Ok(routes)
}


pub async fn interface_name_to_index(handle: &rtnetlink::Handle, name: &str) -> Result<u32, Error> {
    let mut res = handle.link().get().match_name(name.to_string()).execute();

    res.try_next().await?.map(|msg| {
        msg.header.index
    }).ok_or_else(|| {
        Error::InterfaceNotFound(name.to_string())
    })
}

pub async fn get_state(handle: &rtnetlink::Handle, route_proto: u8) -> Result<State, Error> {
    let interfaces = get_vlan_interfaces(handle).await?;
    let addresses = get_addresses(handle).await?;
    let routes = get_routes(handle, route_proto).await?;

    Ok(State {
        interfaces,
        addresses,
        routes
    })
}