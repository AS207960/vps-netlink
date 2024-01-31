use crate::{Error, netlink, config};

#[derive(Debug)]
pub struct AddAddress {
    address: std::net::IpAddr,
    prefix_length: u8,
    interface_name: String,
}

#[derive(Debug)]
pub struct AddRoute {
    destination: std::net::IpAddr,
    destination_prefix_length: u8,
    interface_name: String,
}

#[derive(Debug)]
pub enum Diff {
    AddInterface(netlink::Interface),
    RemoveInterface(u32),
    AddAddress(AddAddress),
    RemoveAddress(netlink_packet_route::address::AddressMessage),
    AddRoute(AddRoute),
    RemoveRoute(netlink_packet_route::route::RouteMessage),
}

#[derive(serde::Serialize)]
pub struct InterfaceState<'a> {
    pub name: String,
    pub vps: &'a config::VPS,
}

pub async fn make_diff<'a>(
    handle: &rtnetlink::Handle, root_interface: &str, target: &'a [config::VPS],
    state: netlink::State,
) -> Result<(Vec<Diff>, Vec<InterfaceState<'a>>), Error> {
    let mut keep_interfaces = vec![];
    let mut keep_routes = vec![];
    let mut rem_addresses = vec![];

    let mut diff_add = vec![];
    let mut diff = vec![];
    let mut interface_states = vec![];

    let mut next_interface_id = state.interfaces.iter().map(|i| {
        usize::from_str_radix(&i.name[3..], 10).unwrap_or(0)
    }).max().unwrap_or(0) + 1;
    let link_interface = netlink::interface_name_to_index(handle, root_interface).await?;

    for vps in target {
        match state.interfaces.iter().find(|i| i.vlan == vps.vlan) {
            Some(i) => {
                keep_interfaces.push(i.index);
                interface_states.push(InterfaceState {
                    name: i.name.clone(),
                    vps,
                });

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
                    diff_add.push(Diff::AddAddress(AddAddress {
                        address: std::net::IpAddr::V4(vps.v4_addr),
                        prefix_length: 31,
                        interface_name: i.name.clone(),
                    }));
                }

                let mut found_v4 = vec![];
                let mut found_v6 = false;

                for route in state.routes.iter().filter(|r| r.interface == i.index) {
                    match route.destination {
                        std::net::IpAddr::V4(dest) => {
                            if let Some(public_v4) = &vps.v4_public {
                                let addrs = public_v4.as_many();
                                if addrs.contains(&dest) && route.destination_prefix_length == 32 {
                                    keep_routes.push(route.message.clone());
                                    found_v4.push(dest);
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

                if let Some(public_v4) = &vps.v4_public {
                    for addr in public_v4.as_many().iter().filter(|p| !found_v4.contains(p)) {
                        diff_add.push(Diff::AddRoute(AddRoute {
                            destination: std::net::IpAddr::V4(*addr),
                            destination_prefix_length: 32,
                            interface_name: i.name.clone(),
                        }));
                    }
                }

                if !found_v6 {
                    diff_add.push(Diff::AddRoute(AddRoute {
                        destination: std::net::IpAddr::V6(vps.v6_prefix),
                        destination_prefix_length: 64,
                        interface_name: i.name.clone(),
                    }));
                }
            },
            None => {
                let id = next_interface_id;
                next_interface_id += 1;
                let interface_name = format!("vps{}", id);

                interface_states.push(InterfaceState {
                    name: interface_name.clone(),
                    vps,
                });

                diff_add.push(Diff::AddInterface(netlink::Interface {
                    name: interface_name.clone(),
                    index: 0,
                    link: link_interface,
                    vlan: vps.vlan
                }));
                diff_add.push(Diff::AddAddress(AddAddress {
                    address: std::net::IpAddr::V4(vps.v4_addr),
                    prefix_length: 31,
                    interface_name: interface_name.clone(),
                }));
                if let Some(public_v4) = &vps.v4_public {
                    for addr in public_v4.as_many() {
                        diff_add.push(Diff::AddRoute(AddRoute {
                            destination: std::net::IpAddr::V4(*addr),
                            destination_prefix_length: 32,
                            interface_name: interface_name.clone(),
                        }));
                    }
                }
                diff_add.push(Diff::AddRoute(AddRoute {
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
    diff.extend(diff_add.into_iter());

    Ok((diff, interface_states))
}

pub async fn apply_diff(handle: &rtnetlink::Handle, route_proto: u8, diff: Vec<Diff>) -> Result<(), Error> {
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
                let interface = netlink::interface_name_to_index(handle, &a.interface_name).await?;
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
                let interface = netlink::interface_name_to_index(handle, &r.interface_name).await?;
                let req = handle.route().add()
                    .protocol(route_proto)
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