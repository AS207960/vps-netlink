use futures_util::TryStreamExt;

const VPS_RT_PROTO: u8 = 200;
const VPS_INTERFACE: &'static str = "eth0";

#[derive(Debug)]
enum Error {
    NetlinkError(rtnetlink::Error),
    Io(std::io::Error),
    InterfaceNotFound(String),
}

impl From<rtnetlink::Error> for Error {
    fn from(value: rtnetlink::Error) -> Self {
        match value {
            rtnetlink::Error::NetlinkError(e) => {
                Self::Io(e.to_io())
            }
            v => Self::NetlinkError(v)
        }
    }
}

struct VPS {
    vlan: u16,
    v4_addr: std::net::Ipv4Addr,
    v4_public: Option<std::net::Ipv4Addr>,
    v6_prefix: std::net::Ipv6Addr,
}

const TARGET_VPS: &'static [VPS] = &[
    VPS {
        vlan: 4000,
        v4_addr: std::net::Ipv4Addr::new(100, 64, 0, 0),
        v4_public: Some(std::net::Ipv4Addr::new(193, 3, 165, 223)),
        v6_prefix: std::net::Ipv6Addr::new(0x2a11, 0xf2c0, 0x3, 0, 0, 0, 0, 0)
    }
];

#[derive(Debug)]
struct Interface {
    name: String,
    index: u32,
    link: u32,
    vlan: u16,
}

#[derive(Debug)]
struct Address {
    interface: u32,
    address: std::net::IpAddr,
    prefix_length: u8,
    message: netlink_packet_route::address::AddressMessage,
}

#[derive(Debug)]
struct Route {
    destination: std::net::IpAddr,
    destination_prefix_length: u8,
    interface: u32,
    message: netlink_packet_route::route::RouteMessage,
}

#[derive(Debug)]
struct AddAddress {
    address: std::net::IpAddr,
    prefix_length: u8,
    interface_name: String,
}

#[derive(Debug)]
struct AddRoute {
    destination: std::net::IpAddr,
    destination_prefix_length: u8,
    interface_name: String,
}

#[derive(Debug)]
enum Diff {
    AddInterface(Interface),
    RemoveInterface(u32),
    AddAddress(AddAddress),
    RemoveAddress(netlink_packet_route::address::AddressMessage),
    AddRoute(AddRoute),
    RemoveRoute(netlink_packet_route::route::RouteMessage),
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


async fn get_routes(handle: &rtnetlink::Handle) -> Result<Vec<Route>, Error> {
    let mut routes = vec![];
    let mut vps_routes = vec![];

    let mut v4_routes = handle.route().get(rtnetlink::IpVersion::V4).execute();
    while let Some(msg) = v4_routes.try_next().await? {
        if msg.header.protocol == VPS_RT_PROTO {
            vps_routes.push(msg);
        }
    }

    let mut v6_routes = handle.route().get(rtnetlink::IpVersion::V6).execute();
    while let Some(msg) = v6_routes.try_next().await? {
        if msg.header.protocol == VPS_RT_PROTO {
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


async fn interface_name_to_index(handle: &rtnetlink::Handle, name: &str) -> Result<u32, Error> {
    let mut res = handle.link().get().match_name(name.to_string()).execute();

    res.try_next().await?.map(|msg| {
        msg.header.index
    }).ok_or_else(|| {
        Error::InterfaceNotFound(name.to_string())
    })
}

#[derive(Debug)]
struct State {
    interfaces: Vec<Interface>,
    addresses: Vec<Address>,
    routes: Vec<Route>,
}

async fn make_diff(
    handle: &rtnetlink::Handle, target: &[VPS],
    state: State
) -> Result<Vec<Diff>, Error> {
    let mut keep_interfaces = vec![];
    let mut keep_routes = vec![];
    let mut rem_addresses = vec![];

    let mut diff = vec![];

    let mut next_interface_id = state.interfaces.iter().map(|i| {
        usize::from_str_radix(&i.name[3..], 10).unwrap_or(0)
    }).max().unwrap_or(0) + 1;
    let link_interface = interface_name_to_index(handle, VPS_INTERFACE).await?;

    for vps in target {
        match state.interfaces.iter().find(|i| i.vlan == vps.vlan) {
            Some(i) => {
                keep_interfaces.push(i.index);

                let mut found_v4_addr = false;

                for address in state.addresses.iter().filter(|a| a.interface == i.index) {
                    match &address.address {
                        std::net::IpAddr::V4(dest) => {
                            if &vps.v4_addr == dest && address.prefix_length == 31 {
                                found_v4_addr = true;
                            } else {
                                rem_addresses.push(address.message.clone());
                            }
                        }
                        std::net::IpAddr::V6(_) => {}
                    }
                }

                if !found_v4_addr {
                    diff.push(Diff::AddAddress(AddAddress {
                        address: std::net::IpAddr::V4(vps.v4_addr),
                        prefix_length: 31,
                        interface_name: i.name.clone(),
                    }));
                }

                let mut found_v4 = false;
                let mut found_v6 = false;

                for route in state.routes.iter().filter(|r| r.interface == i.index) {
                    match route.destination {
                        std::net::IpAddr::V4(dest) => {
                            if let Some(public_v4) = &vps.v4_public {
                                if public_v4 == &dest && route.destination_prefix_length == 32 {
                                    keep_routes.push(route.message.clone());
                                    found_v4 = true;
                                }
                            }
                        }
                        std::net::IpAddr::V6(dest) => {
                            if vps.v6_prefix == dest && route.destination_prefix_length == 64 {
                                keep_routes.push(route.message.clone());
                                found_v6 = true;
                            }
                        }
                    }
                }

                if !found_v4 {
                    if let Some(public_v4) = &vps.v4_public {
                        diff.push(Diff::AddRoute(AddRoute {
                            destination: std::net::IpAddr::V4(public_v4.clone()),
                            destination_prefix_length: 32,
                            interface_name: i.name.clone(),
                        }));
                    }
                }
                if !found_v6 {
                    diff.push(Diff::AddRoute(AddRoute {
                        destination: std::net::IpAddr::V6(vps.v6_prefix.clone()),
                        destination_prefix_length: 64,
                        interface_name: i.name.clone(),
                    }));
                }
            },
            None => {
                let id = next_interface_id;
                next_interface_id += 1;
                let interface_name = format!("vps{}", id);

                diff.push(Diff::AddInterface(Interface {
                    name: interface_name.clone(),
                    index: 0,
                    link: link_interface,
                    vlan: vps.vlan
                }));
                diff.push(Diff::AddAddress(AddAddress {
                    address: std::net::IpAddr::V4(vps.v4_addr),
                    prefix_length: 31,
                    interface_name: interface_name.clone(),
                }));
                if let Some(public_v4) = &vps.v4_public {
                    diff.push(Diff::AddRoute(AddRoute {
                        destination: std::net::IpAddr::V4(public_v4.clone()),
                        destination_prefix_length: 32,
                        interface_name: interface_name.clone(),
                    }));
                }
                diff.push(Diff::AddRoute(AddRoute {
                    destination: std::net::IpAddr::V6(vps.v6_prefix.clone()),
                    destination_prefix_length: 64,
                    interface_name: interface_name.clone(),
                }));
            }
        }
    }

    let mut rem_interfaces = vec![];

    for interface in &state.interfaces {
        if !keep_interfaces.contains(&interface.index) {
            diff.push(Diff::RemoveInterface(interface.index));
            rem_interfaces.push(interface.index);
        }
    }

    for route in &state.routes {
        if !keep_routes.contains(&route.message) && !rem_interfaces.contains(&route.interface) {
            diff.push(Diff::RemoveRoute(route.message.clone()));
        }
    }

    for address in rem_addresses {
        diff.push(Diff::RemoveAddress(address));
    }

    Ok(diff)
}

async fn apply_diff(handle: &rtnetlink::Handle, diff: Vec<Diff>) -> Result<(), Error> {
    for command in diff {
        match command {
            Diff::AddInterface(i) => {
                handle.link().add()
                    .vlan(i.name, i.link, i.vlan)
                    .execute().await?;
            }
            Diff::RemoveInterface(i) => {
                handle.link().del(i).execute().await?;
            }
            Diff::AddAddress(a) => {
                let interface = interface_name_to_index(handle, &a.interface_name).await?;
                handle.address()
                    .add(interface, a.address, a.prefix_length)
                    .execute().await?;
            }
            Diff::RemoveAddress(a) => {
                handle.address()
                    .del(a)
                    .execute().await?;
            }
            Diff::AddRoute(r) => {
                let interface = interface_name_to_index(handle, &r.interface_name).await?;
                let req = handle.route().add()
                    .protocol(VPS_RT_PROTO)
                    .output_interface(interface);
                match r.destination {
                    std::net::IpAddr::V4(v4) => {
                        req.v4()
                            .destination_prefix(v4, r.destination_prefix_length)
                            .execute().await?;
                    }
                    std::net::IpAddr::V6(v6) => {
                        req.v6()
                            .destination_prefix(v6, r.destination_prefix_length)
                            .execute().await?;
                    }
                };
            }
            Diff::RemoveRoute(msg) => {
                handle.route()
                    .del(msg)
                    .execute().await?;
            }
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() {
    let (conn, handle, mut _messages) = rtnetlink::new_connection().unwrap();

    tokio::spawn(conn);

    let interfaces = get_vlan_interfaces(&handle).await.unwrap();
    let addresses = get_addresses(&handle).await.unwrap();
    let routes = get_routes(&handle).await.unwrap();

    let state = State {
        interfaces,
        addresses,
        routes
    };

    let diff = make_diff(&handle, TARGET_VPS, state).await.unwrap();
    println!("State change: {:#?}", diff);
    apply_diff(&handle, diff).await.unwrap();
}
