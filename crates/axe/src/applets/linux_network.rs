use std::collections::HashMap;
use std::ffi::OsString;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use futures_util::TryStreamExt;
use netlink_packet_route::AddressFamily;
use netlink_packet_route::address::{AddressAttribute, AddressFlags, AddressMessage};
use netlink_packet_route::link::{
    LinkAttribute, LinkFlags, LinkInfo as LinkInfoAttribute, LinkLayerType, LinkMessage, Prop,
};
use netlink_packet_route::neighbour::{
    NeighbourAddress, NeighbourAttribute, NeighbourFlags, NeighbourMessage,
};
use netlink_packet_route::route::{
    RouteAddress, RouteAttribute, RouteFlags, RouteMessage, RouteMetric, RouteType,
};
use netlink_packet_route::rule::{RuleAttribute, RuleMessage};
use rtnetlink::IpVersion;

pub type AppletFn = fn(Vec<OsString>) -> i32;

pub fn commands() -> HashMap<String, AppletFn> {
    let mut commands = HashMap::new();
    commands.insert("arp".into(), arp as AppletFn);
    commands.insert("ifconfig".into(), ifconfig as AppletFn);
    commands.insert("ip".into(), ip as AppletFn);
    commands.insert("ipaddr".into(), ipaddr as AppletFn);
    commands.insert("ipcalc".into(), ipcalc as AppletFn);
    commands.insert("iplink".into(), iplink as AppletFn);
    commands.insert("ipneigh".into(), ipneigh as AppletFn);
    commands.insert("iproute".into(), iproute as AppletFn);
    commands.insert("iprule".into(), iprule as AppletFn);
    commands
}

pub fn arp(args: Vec<OsString>) -> i32 {
    run("arp", || run_arp(args))
}

pub fn ifconfig(args: Vec<OsString>) -> i32 {
    run("ifconfig", || run_ifconfig(args))
}

pub fn ip(args: Vec<OsString>) -> i32 {
    run("ip", || run_ip(args, None))
}

pub fn ipaddr(args: Vec<OsString>) -> i32 {
    run("ipaddr", || run_ip(args, Some(Object::Address)))
}

pub fn iplink(args: Vec<OsString>) -> i32 {
    run("iplink", || run_ip(args, Some(Object::Link)))
}

pub fn ipneigh(args: Vec<OsString>) -> i32 {
    run("ipneigh", || run_ip(args, Some(Object::Neighbour)))
}

pub fn iproute(args: Vec<OsString>) -> i32 {
    run("iproute", || run_ip(args, Some(Object::Route)))
}

pub fn iprule(args: Vec<OsString>) -> i32 {
    run("iprule", || run_ip(args, Some(Object::Rule)))
}

fn run(name: &str, operation: impl FnOnce() -> Result<(), String>) -> i32 {
    match operation() {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("{name}: {error}");
            1
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Object {
    Address,
    Link,
    Neighbour,
    Route,
    Rule,
}

#[derive(Default)]
struct IpOptions {
    family: Option<AddressFamily>,
    oneline: bool,
    brief: bool,
    stats: bool,
    json: bool,
    device: Option<String>,
    /// `up` selector for `ip link/addr show`.
    up_only: bool,
    /// `type KIND` selector for `ip link show`.
    link_type: Option<String>,
    /// `master DEVICE` selector for `ip link show`.
    master: Option<String>,
    /// `scope NAME` selector for `ip addr show`.
    scope: Option<String>,
    /// Route table selector: `None` means the iproute2 default (main),
    /// `Some("all")` means every table.
    route_table: Option<String>,
    route_default_only: bool,
    route_protocol: Option<String>,
    route_prefix: Option<String>,
    neigh_state: Option<String>,
    neigh_address: Option<String>,
}

struct RouteGet {
    destination: IpAddr,
    source: Option<IpAddr>,
    mark: Option<u32>,
}

fn run_ip(args: Vec<OsString>, forced: Option<Object>) -> Result<(), String> {
    uucore::panic::mute_sigpipe_panic();
    let mut values = utf8_args(args)?;
    let program = values.first().cloned().unwrap_or_else(|| "ip".into());
    values.remove(0);

    if values
        .iter()
        .any(|arg| arg == "-h" || arg == "--help" || arg == "help")
    {
        println!(
            "Usage: {program} [ OPTIONS ] OBJECT {{ show | list }}\n       \
             {program} route get ADDRESS [from ADDRESS] [mark MARK] [dev IFACE]\n       \
             {program} {{ address | link | neighbour | route | rule }} show\n\
             Options: -4 -6 -o -br -s -j -d"
        );
        return Ok(());
    }

    let mut options = IpOptions::default();
    let mut positional = Vec::new();
    let mut values = values.into_iter().peekable();
    while let Some(value) = values.next() {
        match value.as_str() {
            "-4" => options.family = Some(AddressFamily::Inet),
            "-6" => options.family = Some(AddressFamily::Inet6),
            "-o" | "--oneline" => options.oneline = true,
            "-br" | "--brief" => options.brief = true,
            "-s" | "-stats" | "--stats" | "--statistics" => options.stats = true,
            "-j" | "--json" => options.json = true,
            "-d" | "--details" => {}
            "-p" | "--pretty" => {}
            "-c" | "-color" | "--color" => {}
            "-N" | "--netnsid" => {}
            "-0" | "-r" | "--resolve" => {}
            "-V" | "-Version" | "--version" => {
                println!("ip (axe) {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "-f" | "-family" | "--family" => match values.next().as_deref() {
                Some("inet") | Some("ipv4") => options.family = Some(AddressFamily::Inet),
                Some("inet6") | Some("ipv6") => options.family = Some(AddressFamily::Inet6),
                Some("link") | Some("bridge") | Some("mpls") => {}
                Some(other) => {
                    return Err(format!("argument of \"{other}\" is not a valid family"));
                }
                None => return Err("option requires an argument -- 'f'".into()),
            },
            value if value.starts_with("-f") && matches!(&value[1..], "inet" | "inet6") => {
                options.family = Some(if &value[1..] == "inet" {
                    AddressFamily::Inet
                } else {
                    AddressFamily::Inet6
                });
            }
            value if value.starts_with('-') => return Err(format!("unknown option '{value}'")),
            _ => positional.push(value),
        }
    }

    let object = if let Some(object) = forced {
        object
    } else if positional
        .first()
        .is_none_or(|value| value == "show" || value == "list")
    {
        Object::Address
    } else {
        let value = positional.remove(0);
        parse_object(&value)?
    };

    let route_get = if object == Object::Route
        && positional
            .first()
            .is_some_and(|value| "get".starts_with(value))
    {
        positional.remove(0);
        if positional.first().is_some_and(|value| value == "to") {
            positional.remove(0);
        }
        let destination = parse_shorthand_address(
            positional
                .first()
                .ok_or("route get requires a destination")?,
        )?;
        positional.remove(0);
        let mut source = None;
        let mut mark = None;
        while !positional.is_empty() {
            let keyword = positional.remove(0);
            let value = positional
                .first()
                .ok_or_else(|| format!("'{keyword}' requires an argument"))?
                .clone();
            positional.remove(0);
            match keyword.as_str() {
                "from" => {
                    source = Some(parse_shorthand_address(&value)?);
                }
                "mark" => {
                    mark = Some(
                        value
                            .parse()
                            .map_err(|_| format!("invalid mark value '{value}'"))?,
                    );
                }
                "dev" | "oif" => options.device = Some(value),
                keyword => return Err(format!("unsupported route get selector '{keyword}'")),
            }
        }
        Some(RouteGet {
            destination,
            source,
            mark,
        })
    } else {
        if positional
            .first()
            .is_some_and(|value| "show".starts_with(value) || "list".starts_with(value))
        {
            positional.remove(0);
        }
        parse_show_selectors(object, &mut positional, &mut options)?;
        None
    };

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(async move {
        let (snapshot, handle) = Snapshot::load().await?;
        if matches!(object, Object::Address | Object::Link)
            && let Some(name) = options.device.as_deref()
            && !snapshot
                .links
                .iter()
                .any(|link| link_matches(link, &options))
        {
            return Err(format!("Device \"{name}\" does not exist."));
        }
        match object {
            Object::Address => show_addresses(&snapshot, &options),
            Object::Link => show_links(&snapshot, &options),
            Object::Neighbour => show_neighbours(&snapshot, &options),
            Object::Route => {
                if let Some(query) = route_get {
                    route_get_netlink(&handle, &snapshot, &options, &query).await
                } else {
                    show_routes(&snapshot, &options)
                }
            }
            Object::Rule => show_rules(&snapshot, &options),
        }
    })
}

/// Parses the selectors iproute2 accepts after `show`/`list`.
fn parse_show_selectors(
    object: Object,
    positional: &mut Vec<String>,
    options: &mut IpOptions,
) -> Result<(), String> {
    if positional.first().is_some_and(|value| value == "dev") {
        positional.remove(0);
        options.device = positional.first().cloned();
        if options.device.is_none() {
            return Err("'dev' requires an interface name".into());
        }
        positional.remove(0);
    }
    let mut index = 0;
    while index < positional.len() {
        let value = positional[index].clone();
        let take_argument = |index: &mut usize| -> Option<String> {
            let argument = positional.get(*index + 1).cloned();
            if argument.is_some() {
                *index += 1;
            }
            argument
        };
        match (object, value.as_str()) {
            (Object::Link | Object::Address, "up") => options.up_only = true,
            (Object::Link, "type") => {
                if let Some(kind) = take_argument(&mut index) {
                    options.link_type = Some(kind);
                }
            }
            (Object::Link, "master") => {
                if let Some(master) = take_argument(&mut index) {
                    options.master = Some(master);
                }
            }
            (Object::Address, "scope") => {
                if let Some(scope) = take_argument(&mut index) {
                    options.scope = Some(scope);
                }
            }
            (Object::Route, "default") => options.route_default_only = true,
            (Object::Route, "table") => {
                let table = positional
                    .get(index + 1)
                    .cloned()
                    .ok_or("'table' requires an argument")?;
                index += 1;
                options.route_table = Some(table);
            }
            (Object::Route, "proto") => {
                if let Some(protocol) = take_argument(&mut index) {
                    options.route_protocol = Some(protocol);
                }
            }
            (Object::Route, "match") => {
                if let Some(prefix) = take_argument(&mut index) {
                    options.route_prefix = Some(prefix);
                }
            }
            (Object::Route, value) if value.parse::<IpNet>().is_ok() => {
                options.route_prefix = Some(value.to_string());
            }
            (Object::Neighbour, "nud") => {
                if let Some(state) = take_argument(&mut index) {
                    options.neigh_state = Some(state);
                }
            }
            (Object::Neighbour, "to" | "dev" | "vrf") => {
                take_argument(&mut index);
            }
            (Object::Neighbour, value) if value.contains(':') => {
                options.neigh_address = Some(value.to_string());
            }
            (Object::Address | Object::Link, value)
                if options.device.is_none() && !value.starts_with('-') =>
            {
                options.device = Some(value.to_string());
            }
            (_, value) => {
                return Err(format!(
                    "unsupported selector '{value}' (only show/list operations are supported)"
                ));
            }
        }
        index += 1;
    }
    Ok(())
}

/// Parses an address the way `getaddr` does for route lookups, accepting the
/// numeric shorthand forms `10.1` and `10` for IPv4.
fn parse_shorthand_address(value: &str) -> Result<IpAddr, String> {
    if let Ok(address) = value.parse::<IpAddr>() {
        return Ok(address);
    }
    if !value.is_empty()
        && value.len() <= 15
        && value
            .split('.')
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
    {
        let mut parts: Vec<u8> = value
            .split('.')
            .map(|part| {
                part.parse()
                    .map_err(|_| format!("invalid destination '{value}'"))
            })
            .collect::<Result<_, _>>()?;
        while parts.len() < 4 {
            parts.insert(0, 0);
        }
        if parts.len() == 4 {
            return Ok(IpAddr::V4(Ipv4Addr::new(
                parts[0], parts[1], parts[2], parts[3],
            )));
        }
    }
    Err(format!("invalid destination '{value}'"))
}

fn parse_object(value: &str) -> Result<Object, String> {
    // iproute2 accepts any unambiguous prefix of an object name.
    const OBJECTS: [(&str, Object); 5] = [
        ("address", Object::Address),
        ("link", Object::Link),
        ("neighbour", Object::Neighbour),
        ("route", Object::Route),
        ("rule", Object::Rule),
    ];
    if value == "neighbor" {
        return Ok(Object::Neighbour);
    }
    for (name, object) in OBJECTS {
        if name.starts_with(value) {
            return Ok(object);
        }
    }
    Err(format!(
        "Object \"{value}\" is unknown, try \"{value} help\"."
    ))
}

/// A parsed `A.B.C.D/N` prefix used by the route `match` selector.
struct IpNet(IpAddr, u8);

impl std::str::FromStr for IpNet {
    type Err = ();
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (address, prefix) = value.split_once('/').ok_or(())?;
        let address = address.parse::<IpAddr>().map_err(|_| ())?;
        let prefix = prefix.parse::<u8>().map_err(|_| ())?;
        let width = if address.is_ipv4() { 32 } else { 128 };
        if prefix > width {
            return Err(());
        }
        Ok(IpNet(address, prefix))
    }
}

struct Snapshot {
    links: Vec<LinkMessage>,
    addresses: Vec<AddressMessage>,
    neighbours: Vec<NeighbourMessage>,
    routes: Vec<RouteMessage>,
    rules: Vec<RuleMessage>,
    names: HashMap<u32, String>,
}

impl Snapshot {
    async fn load() -> Result<(Self, rtnetlink::Handle), String> {
        let (connection, handle, _) =
            rtnetlink::new_connection().map_err(|error| error.to_string())?;
        tokio::spawn(connection);

        let mut links = Vec::new();
        let mut link_stream = handle.link().get().execute();
        while let Some(link) = link_stream
            .try_next()
            .await
            .map_err(|error| error.to_string())?
        {
            links.push(link);
        }

        let names = links
            .iter()
            .filter_map(|link| {
                link.attributes
                    .iter()
                    .find_map(|attribute| match attribute {
                        LinkAttribute::IfName(name) => Some((link.header.index, name.clone())),
                        _ => None,
                    })
            })
            .collect();

        let mut addresses = Vec::new();
        let mut address_stream = handle.address().get().execute();
        while let Some(address) = address_stream
            .try_next()
            .await
            .map_err(|error| error.to_string())?
        {
            addresses.push(address);
        }

        let mut neighbours = Vec::new();
        let mut neighbour_stream = handle.neighbours().get().execute();
        while let Some(neighbour) = neighbour_stream
            .try_next()
            .await
            .map_err(|error| error.to_string())?
        {
            neighbours.push(neighbour);
        }

        let mut routes = Vec::new();
        for family in [AddressFamily::Inet, AddressFamily::Inet6] {
            let mut request = RouteMessage::default();
            request.header.address_family = family;
            let mut route_stream = handle.route().get(request).execute();
            while let Some(route) = route_stream
                .try_next()
                .await
                .map_err(|error| error.to_string())?
            {
                routes.push(route);
            }
        }

        let mut rules = Vec::new();
        for version in [IpVersion::V4, IpVersion::V6] {
            let mut rule_stream = handle.rule().get(version).execute();
            while let Some(rule) = rule_stream
                .try_next()
                .await
                .map_err(|error| error.to_string())?
            {
                rules.push(rule);
            }
        }

        Ok((
            Self {
                links,
                addresses,
                neighbours,
                routes,
                rules,
                names,
            },
            handle,
        ))
    }
}

fn master_filter(snapshot: &Snapshot, options: &IpOptions) -> Option<u32> {
    let master = options.master.as_deref()?;
    Some(
        snapshot
            .links
            .iter()
            .find(|link| {
                link.attributes.iter().any(|attribute| match attribute {
                    LinkAttribute::IfName(name) => name == master,
                    _ => false,
                })
            })
            .map(|link| link.header.index)
            .unwrap_or(u32::MAX),
    )
}

fn link_under_master(link: &LinkMessage, master_index: Option<u32>) -> bool {
    master_index.is_none_or(|expected| {
        link.attributes.iter().any(
            |attribute| matches!(attribute, LinkAttribute::Controller(index) if *index == expected),
        )
    })
}

fn print_link_header(link: &LinkMessage, info: &LinkInfo) {
    let flags = ip_flags(link, info).join(",");
    let link_type = link.header.link_layer_type.to_string().to_ascii_lowercase();
    // ipaddr output carries no mode field; iplink prints it.
    let mut line = format!(
        "{}: {}: <{}> mtu {}",
        link.header.index,
        info.name,
        flags,
        info.mtu.unwrap_or_default()
    );
    if let Some(qdisc) = &info.qdisc {
        line.push_str(&format!(" qdisc {qdisc}"));
    }
    line.push_str(&format!(" state {}", info.state));
    line.push_str(" group default");
    if let Some(queue_len) = info.tx_queue_len {
        line.push_str(&format!(" qlen {queue_len}"));
    }
    match &info.address {
        Some(address) => {
            line.push_str(&format!(
                "\n    link/{link_type} {address} brd {}",
                info.broadcast.clone().unwrap_or_default()
            ));
            if let Some(permanent) = &info.permanent_address
                && permanent != address
            {
                line.push_str(&format!(" permaddr {permanent}"));
            }
        }
        None if link_type == "none" => line.push_str("\n    link/none "),
        _ => {}
    }
    for altname in &info.alternate_names {
        line.push_str(&format!("\n    altname {altname}"));
    }
    println!("{line}");
}

fn show_links(snapshot: &Snapshot, options: &IpOptions) -> Result<(), String> {
    let master_index = master_filter(snapshot, options);
    let links = snapshot
        .links
        .iter()
        .filter(|link| link_matches(link, options))
        .filter(|link| link_under_master(link, master_index))
        .collect::<Vec<_>>();
    if options.json {
        let values = links
            .iter()
            .map(|link| serde_json::Value::Object(link_json(link)))
            .collect::<Vec<_>>();
        println!(
            "{}",
            serde_json::to_string(&values).map_err(|error| error.to_string())?
        );
        return Ok(());
    }

    for link in links {
        let info = link_info(link);
        if options.brief {
            println!(
                "{:<16} {:<12} {}",
                info.name,
                info.state,
                info.address.unwrap_or_default()
            );
            continue;
        }
        let flags = ip_flags(link, &info).join(",");
        let link_type = link.header.link_layer_type.to_string().to_ascii_lowercase();
        let mode = if info.mode.eq_ignore_ascii_case("default") {
            None
        } else {
            Some(info.mode.to_uppercase())
        };
        let mut line = format!(
            "{}: {}: <{}> mtu {}",
            link.header.index,
            info.name,
            flags,
            info.mtu.unwrap_or_default()
        );
        if let Some(qdisc) = &info.qdisc {
            line.push_str(&format!(" qdisc {qdisc}"));
        }
        line.push_str(&format!(" state {}", info.state));
        line.push_str(&format!(
            " mode {}",
            mode.unwrap_or_else(|| "DEFAULT".into())
        ));
        line.push_str(" group default");
        if let Some(queue_len) = info.tx_queue_len {
            line.push_str(&format!(" qlen {queue_len}"));
        }
        match &info.address {
            Some(address) => {
                line.push_str(&format!(
                    "\n    link/{link_type} {address} brd {}",
                    info.broadcast.clone().unwrap_or_default()
                ));
                if let Some(permanent) = &info.permanent_address
                    && permanent != address
                {
                    line.push_str(&format!(" permaddr {permanent}"));
                }
            }
            None if link_type == "none" => line.push_str("\n    link/none "),
            _ => {}
        }
        for altname in &info.alternate_names {
            line.push_str(&format!("\n    altname {altname}"));
        }
        if options.oneline {
            println!("{}", line.replace('\n', "\\"));
        } else {
            println!("{line}");
        }
        if options.stats
            && let Some(stats) = info.stats
        {
            println!(
                "    RX: bytes  packets  errors  dropped\n    {:<10} {:<8} {:<7} {}",
                stats.rx_bytes, stats.rx_packets, stats.rx_errors, stats.rx_dropped
            );
            println!(
                "    TX: bytes  packets  errors  dropped\n    {:<10} {:<8} {:<7} {}",
                stats.tx_bytes, stats.tx_packets, stats.tx_errors, stats.tx_dropped
            );
        }
    }
    Ok(())
}

fn show_addresses(snapshot: &Snapshot, options: &IpOptions) -> Result<(), String> {
    let master_index = master_filter(snapshot, options);
    let selected = snapshot
        .links
        .iter()
        .filter(|link| link_matches(link, options))
        .filter(|link| link_under_master(link, master_index))
        .collect::<Vec<_>>();
    if options.json {
        let values = selected
            .iter()
            .map(|link| {
                let addresses = snapshot
                    .addresses
                    .iter()
                    .filter(|address| {
                        address.header.index == link.header.index
                            && family_matches(address.header.family, options.family)
                    })
                    .filter_map(address_json)
                    .map(serde_json::Value::Object)
                    .collect::<Vec<_>>();
                let mut value = link_json(link);
                value.insert("addr_info".into(), addresses.into());
                serde_json::Value::Object(value)
            })
            .collect::<Vec<_>>();
        println!(
            "{}",
            serde_json::to_string(&values).map_err(|error| error.to_string())?
        );
        return Ok(());
    }

    for link in selected {
        let info = link_info(link);
        let addresses = snapshot
            .addresses
            .iter()
            .filter(|address| {
                address.header.index == link.header.index
                    && family_matches(address.header.family, options.family)
            })
            .filter_map(address_text)
            .filter(|address| {
                options
                    .scope
                    .as_deref()
                    .is_none_or(|expected| address.scope == expected)
            })
            .collect::<Vec<_>>();
        if options.brief {
            let values = addresses
                .iter()
                .map(|address| format!("{}/{}", address.address, address.prefix))
                .collect::<Vec<_>>()
                .join(" ");
            if values.is_empty() {
                println!("{:<16} {:<14} ", info.name, info.state);
            } else {
                println!("{:<16} {:<14} {} ", info.name, info.state, values);
            }
            continue;
        }
        if options.family.is_some() && addresses.is_empty() {
            continue;
        }
        if !options.oneline {
            print_link_header(link, &info);
        }
        for address in addresses {
            let kind = if address.address.is_ipv4() {
                "inet"
            } else {
                "inet6"
            };
            let mut line = if options.oneline {
                format!(
                    "{}: {}    {kind} {}/{}",
                    link.header.index, info.name, address.address, address.prefix
                )
            } else {
                format!("    {kind} {}/{}", address.address, address.prefix)
            };
            if let Some(broadcast) = &address.broadcast
                && address.address.is_ipv4()
            {
                line.push_str(&format!(" brd {broadcast}"));
            }
            line.push_str(&format!(" scope {}", address.scope));
            let ipv6 = address.address.is_ipv6();
            if address.secondary {
                line.push_str(if ipv6 { " temporary" } else { " secondary" });
            }
            if address.deprecated {
                line.push_str(" deprecated");
            }
            if address.dynamic {
                line.push_str(" dynamic");
            }
            if address.managed_temporary {
                line.push_str(" mngtmpaddr");
            }
            if address.noprefixroute {
                line.push_str(" noprefixroute");
            }
            match &address.label {
                Some(label) => line.push_str(&format!(" {label}")),
                None if !ipv6 => line.push_str(&format!(" {}", info.name)),
                None => line.push(' '),
            }
            if let (Some(valid), Some(preferred)) = (address.valid_lft, address.preferred_lft) {
                if options.oneline {
                    println!(
                        "{line}\\       valid_lft {} preferred_lft {}",
                        format_lifetime(valid),
                        format_lifetime(preferred)
                    );
                } else {
                    println!("{line}");
                    println!(
                        "       valid_lft {} preferred_lft {}",
                        format_lifetime(valid),
                        format_lifetime(preferred)
                    );
                }
            } else {
                println!("{line}");
            }
        }
    }
    Ok(())
}

fn format_lifetime(seconds: u32) -> String {
    if seconds == u32::MAX {
        "forever".into()
    } else {
        format!("{seconds}sec")
    }
}

fn show_neighbours(snapshot: &Snapshot, options: &IpOptions) -> Result<(), String> {
    let show_all_states = options
        .neigh_state
        .as_deref()
        .is_some_and(|state| state.eq_ignore_ascii_case("all"));
    let values = snapshot
        .neighbours
        .iter()
        .filter_map(|message| {
            let destination = message
                .attributes
                .iter()
                .find_map(|attribute| match attribute {
                    NeighbourAttribute::Destination(address) => neighbour_ip(address),
                    _ => None,
                })?;
            if !family_matches(message.header.family, options.family) {
                return None;
            }
            let states = neighbour_states(message);
            // iproute2 hides NONE and NOARP entries unless requested.
            if !show_all_states
                && (states.contains(&"NONE".to_string()) || states.contains(&"NOARP".to_string()))
            {
                return None;
            }
            if let Some(expected) = options.neigh_state.as_deref()
                && !expected.eq_ignore_ascii_case("all")
                && !states
                    .iter()
                    .any(|state| state.eq_ignore_ascii_case(expected))
            {
                return None;
            }
            if let Some(expected) = options.neigh_address.as_deref()
                && destination.to_string() != expected
            {
                return None;
            }
            let device = snapshot
                .names
                .get(&message.header.ifindex)
                .cloned()
                .unwrap_or_else(|| message.header.ifindex.to_string());
            if options
                .device
                .as_deref()
                .is_some_and(|expected| expected != device)
            {
                return None;
            }
            let lladdr = message
                .attributes
                .iter()
                .find_map(|attribute| match attribute {
                    NeighbourAttribute::LinkLayerAddress(bytes) => Some(mac(bytes)),
                    _ => None,
                });
            let router = message.header.flags.contains(NeighbourFlags::Router);
            Some((destination, device, lladdr, states, router))
        })
        .collect::<Vec<_>>();

    if options.json {
        let json = values
            .iter()
            .map(|(dst, dev, lladdr, states, router)| {
                let mut states: Vec<serde_json::Value> =
                    states.iter().map(|state| state.clone().into()).collect();
                if *router {
                    states.push("ROUTER".into());
                }
                serde_json::json!({"dst": dst, "dev": dev, "lladdr": lladdr, "state": states})
            })
            .collect::<Vec<_>>();
        println!(
            "{}",
            serde_json::to_string(&json).map_err(|error| error.to_string())?
        );
    } else {
        for (destination, device, lladdr, states, router) in values {
            print!("{destination} dev {device}");
            if let Some(lladdr) = lladdr {
                print!(" lladdr {lladdr}");
            }
            if router {
                print!(" router");
            }
            println!(" {} ", states.join(" "));
        }
    }
    Ok(())
}

fn neighbour_states(message: &NeighbourMessage) -> Vec<String> {
    message
        .header
        .state
        .to_string()
        .split('|')
        .map(|state| state.to_uppercase())
        .collect()
}

/// iproute2 `ip route` shows the IPv4 main table unless `-6` or a `table`
/// selector says otherwise.
fn route_family(options: &IpOptions) -> Option<AddressFamily> {
    options.family.or(Some(AddressFamily::Inet))
}

fn route_table_selected(options: &IpOptions, table: u32) -> bool {
    match options.route_table.as_deref() {
        None => table == 254,
        Some("all") => true,
        Some("main") => table == 254,
        Some("local") => table == 255,
        Some("default") => table == 253,
        Some(name) => name.parse().is_ok_and(|value: u32| value == table),
    }
}

fn show_routes(snapshot: &Snapshot, options: &IpOptions) -> Result<(), String> {
    let routes = snapshot
        .routes
        .iter()
        .filter(|route| family_matches(route.header.address_family, route_family(options)))
        // The kernel route cache surfaces as RTM_F_CLONED entries; iproute2
        // only lists them for `ip route show cache`.
        .filter(|route| !route.header.flags.contains(RouteFlags::Cloned))
        .filter_map(|route| route_text(snapshot, route, options))
        .collect::<Vec<_>>();

    if options.json {
        let json = routes
            .iter()
            .map(|entry| {
                serde_json::json!({
                    "dst": entry.destination,
                    "gateway": entry.gateway,
                    "dev": entry.device,
                    "protocol": entry.protocol,
                    "prefsrc": entry.source,
                    "metric": entry.metric.filter(|metric| *metric != 0),
                })
            })
            .collect::<Vec<_>>();
        println!(
            "{}",
            serde_json::to_string(&json).map_err(|error| error.to_string())?
        );
    } else {
        for entry in routes {
            print!("{entry} ");
            println!();
        }
    }
    Ok(())
}

/// One rendered routing-table entry with iproute2's field order.
struct RouteEntry {
    destination: String,
    gateway: Option<IpAddr>,
    device: Option<String>,
    source: Option<IpAddr>,
    metric: Option<u32>,
    mtu: Option<u32>,
    protocol: String,
    multipath: Vec<String>,
    scope: Option<String>,
}

impl std::fmt::Display for RouteEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.destination)?;
        if let Some(gateway) = self.gateway {
            write!(f, " via {gateway}")?;
        }
        if let Some(device) = &self.device {
            write!(f, " dev {device}")?;
        }
        if self.protocol != "boot" {
            write!(f, " proto {}", self.protocol)?;
        }
        if let Some(scope) = &self.scope {
            write!(f, " scope {scope}")?;
        }
        if let Some(source) = self.source {
            write!(f, " src {source}")?;
        }
        if let Some(mtu) = self.mtu {
            write!(f, " mtu {mtu}")?;
        }
        if let Some(metric) = self.metric.filter(|metric| *metric != 0) {
            write!(f, " metric {metric}")?;
        }
        for nexthop in &self.multipath {
            write!(f, "\n{nexthop}")?;
        }
        Ok(())
    }
}

fn route_text(
    snapshot: &Snapshot,
    route: &RouteMessage,
    options: &IpOptions,
) -> Option<RouteEntry> {
    let mut destination = None;
    let mut gateway = None;
    let mut source = None;
    let mut device = None;
    let mut metric = None;
    let mut mtu = None;
    let mut table = u32::from(route.header.table);
    let mut multipath = Vec::new();
    for attribute in &route.attributes {
        match attribute {
            RouteAttribute::Destination(value) => destination = route_ip(value),
            RouteAttribute::Gateway(value) => gateway = route_ip(value),
            RouteAttribute::PrefSource(value) => source = route_ip(value),
            RouteAttribute::Oif(index) => device = snapshot.names.get(index).cloned(),
            RouteAttribute::Priority(value) => metric = Some(*value),
            RouteAttribute::Metrics(metrics) => {
                for metric in metrics {
                    if let RouteMetric::Mtu(value) = metric {
                        mtu = Some(*value);
                    }
                }
            }
            RouteAttribute::Table(value) => table = *value,
            RouteAttribute::MultiPath(next_hops) => {
                for nexthop in next_hops {
                    let mut text = String::from("nexthop");
                    if let Some(via) =
                        nexthop
                            .attributes
                            .iter()
                            .find_map(|attribute| match attribute {
                                RouteAttribute::Gateway(value) => route_ip(value),
                                _ => None,
                            })
                    {
                        text.push_str(&format!(" via {via}"));
                    }
                    if let Some(name) = snapshot.names.get(&nexthop.interface_index) {
                        text.push_str(&format!(" dev {name}"));
                    }
                    text.push_str(&format!(" weight {}", u32::from(nexthop.hops) + 1));
                    multipath.push(text);
                }
            }
            _ => {}
        }
    }
    if !route_table_selected(options, table) {
        return None;
    }
    if options
        .device
        .as_deref()
        .is_some_and(|expected| device.as_deref() != Some(expected))
    {
        return None;
    }
    if options.route_default_only && destination.is_some() {
        return None;
    }
    if let Some(protocol) = options.route_protocol.as_deref()
        && route.header.protocol.to_string().to_ascii_lowercase() != protocol
    {
        return None;
    }
    if let Some(selector) = options.route_prefix.as_deref() {
        let selector = selector.parse::<IpNet>().ok()?;
        let address = destination?;
        if !prefix_within(selector, address, route.header.destination_prefix_length) {
            return None;
        }
    }

    let prefix = route.header.destination_prefix_length;
    let host_route = destination.is_some_and(|address| {
        (address.is_ipv4() && prefix == 32) || (address.is_ipv6() && prefix == 128)
    });
    let destination = destination
        .map(|address| {
            if host_route {
                address.to_string()
            } else {
                format!("{address}/{prefix}")
            }
        })
        .unwrap_or_else(|| "default".into());
    let kind = route.header.kind;
    let destination = match kind {
        RouteType::BlackHole => format!("blackhole {destination}"),
        RouteType::Unreachable => format!("unreachable {destination}"),
        RouteType::Prohibit => format!("prohibit {destination}"),
        RouteType::Local if table == 255 => format!("local {destination}"),
        RouteType::Broadcast if table == 255 => format!("broadcast {destination}"),
        RouteType::Multicast if table == 255 => format!("multicast {destination}"),
        _ => destination,
    };
    let scope = match route.header.scope.to_string() {
        scope if scope == "universe" => None,
        scope => Some(scope),
    };
    Some(RouteEntry {
        destination,
        gateway,
        device,
        source,
        metric,
        mtu,
        protocol: route.header.protocol.to_string().to_ascii_lowercase(),
        multipath,
        scope,
    })
}

async fn route_get_netlink(
    handle: &rtnetlink::Handle,
    snapshot: &Snapshot,
    options: &IpOptions,
    query: &RouteGet,
) -> Result<(), String> {
    let family = if query.destination.is_ipv4() {
        AddressFamily::Inet
    } else {
        AddressFamily::Inet6
    };
    if options.family.is_some_and(|expected| expected != family) {
        return Err("address family does not match destination".into());
    }

    let mut message = RouteMessage::default();
    message.header.address_family = family;
    message
        .attributes
        .push(RouteAttribute::Destination(match query.destination {
            IpAddr::V4(address) => RouteAddress::Inet(address),
            IpAddr::V6(address) => RouteAddress::Inet6(address),
        }));
    if let Some(source) = query.source {
        message
            .attributes
            .push(RouteAttribute::Source(match source {
                IpAddr::V4(address) => RouteAddress::Inet(address),
                IpAddr::V6(address) => RouteAddress::Inet6(address),
            }));
    }
    if let Some(mark) = query.mark {
        message.attributes.push(RouteAttribute::Mark(mark));
    }

    let mut stream = handle.route().get(message).execute();
    let route = match stream.try_next().await {
        Ok(Some(route)) => route,
        Ok(None) => return Err(format!("no route to {}", query.destination)),
        Err(error) => return Err(format!("RTNETLINK answers: {error}")),
    };

    let mut destination = query.destination.to_string();
    let mut gateway = None;
    let mut device = None;
    let mut source = None;
    for attribute in &route.attributes {
        match attribute {
            RouteAttribute::Destination(value) => {
                if let Some(address) = route_ip(value) {
                    destination = address.to_string();
                }
            }
            RouteAttribute::Gateway(value) => gateway = route_ip(value),
            RouteAttribute::PrefSource(value) => source = route_ip(value),
            RouteAttribute::Oif(index) => {
                device = snapshot.names.get(index).cloned();
            }
            _ => {}
        }
    }
    if options.json {
        println!(
            "{}",
            serde_json::json!([{
                "dst": destination,
                "gateway": gateway,
                "dev": device,
                "prefsrc": source,
            }])
        );
        return Ok(());
    }

    print!("{destination}");
    if let Some(source) = query.source {
        print!(" from {source}");
    }
    if let Some(gateway) = gateway {
        print!(" via {gateway}");
    }
    if let Some(device) = device {
        print!(" dev {device}");
    }
    if let Some(source) = source {
        print!(" src {source}");
    }
    println!();
    Ok(())
}

fn prefix_matches(address: IpAddr, network: IpAddr, prefix: u8) -> bool {
    match (address, network) {
        (IpAddr::V4(address), IpAddr::V4(network)) => {
            let mask = if prefix == 0 {
                0
            } else {
                u32::MAX << (32 - prefix)
            };
            u32::from(address) & mask == u32::from(network) & mask
        }
        (IpAddr::V6(address), IpAddr::V6(network)) => {
            let mask = if prefix == 0 {
                0
            } else {
                u128::MAX << (128 - prefix)
            };
            u128::from(address) & mask == u128::from(network) & mask
        }
        _ => false,
    }
}

/// Reports whether `network/prefix` falls inside the `selector` prefix.
fn prefix_within(selector: IpNet, network: IpAddr, prefix: u8) -> bool {
    prefix_matches(network, selector.0, selector.1) && prefix >= selector.1
}

fn show_rules(snapshot: &Snapshot, options: &IpOptions) -> Result<(), String> {
    // iproute2 lists IPv4 rules unless `-6` narrows the family.
    let family = options.family.or(Some(AddressFamily::Inet));
    let rules = snapshot
        .rules
        .iter()
        .filter(|rule| family_matches(rule.header.family, family))
        .map(|rule| {
            let mut priority = 0;
            let mut source = "all".to_string();
            let mut destination = None;
            let mut table = u32::from(rule.header.table);
            let mut iif = None;
            let mut oif = None;
            let mut fwmark = None;
            for attribute in &rule.attributes {
                match attribute {
                    RuleAttribute::Priority(value) => priority = *value,
                    RuleAttribute::Source(value) => {
                        source = format!("{value}/{}", rule.header.src_len)
                    }
                    RuleAttribute::Destination(value) => {
                        destination = Some(format!("{value}/{}", rule.header.dst_len))
                    }
                    RuleAttribute::Table(value) => table = *value,
                    RuleAttribute::Iifname(value) => iif = Some(value.clone()),
                    RuleAttribute::Oifname(value) => oif = Some(value.clone()),
                    RuleAttribute::FwMark(value) => fwmark = Some(*value),
                    _ => {}
                }
            }
            (priority, source, destination, table, iif, oif, fwmark)
        })
        .collect::<Vec<_>>();

    if options.json {
        let json = rules.iter().map(|(priority, source, destination, table, iif, oif, fwmark)| serde_json::json!({"priority": priority, "src": source, "dst": destination, "table": table_name(*table), "iif": iif, "oif": oif, "fwmark": fwmark})).collect::<Vec<_>>();
        println!(
            "{}",
            serde_json::to_string(&json).map_err(|error| error.to_string())?
        );
    } else {
        for (priority, source, destination, table, iif, oif, fwmark) in rules {
            print!("{priority}:\tfrom {source}");
            if let Some(destination) = destination {
                print!(" to {destination}");
            }
            if let Some(mark) = fwmark {
                print!(" fwmark 0x{mark:x}");
            }
            if let Some(iif) = iif {
                print!(" iif {iif}");
            }
            if let Some(oif) = oif {
                print!(" oif {oif}");
            }
            println!(" lookup {}", table_name(table));
        }
    }
    Ok(())
}

fn run_arp(args: Vec<OsString>) -> Result<(), String> {
    let values = utf8_args(args)?;
    let mut numeric = false;
    let mut bsd = false;
    let mut device = None;
    let mut host = None;
    let mut iter = values.into_iter().skip(1);
    while let Some(value) = iter.next() {
        match value.as_str() {
            "-n" | "--numeric" => numeric = true,
            "-a" | "--all" => bsd = true,
            "-e" => bsd = false,
            "-i" | "--device" => {
                device = Some(iter.next().ok_or("option requires an argument -- 'i'")?)
            }
            "-v" | "--verbose" => {}
            "-h" | "--help" => {
                println!("Usage: arp [-vn] [-H type] [-i if] [-a] [hostname]");
                return Ok(());
            }
            value if value.starts_with('-') => {
                return Err(format!("unsupported option '{value}' (display mode only)"));
            }
            _ if host.is_none() => host = Some(value),
            _ => return Err("too many arguments".into()),
        }
    }

    let mut options = IpOptions {
        family: Some(AddressFamily::Inet),
        ..IpOptions::default()
    };
    options.device = device;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(async move {
        let (snapshot, _handle) = Snapshot::load().await?;
        let entries = snapshot.neighbours.iter().filter_map(|message| {
            if message.header.family != AddressFamily::Inet { return None; }
            let ip = message.attributes.iter().find_map(|attribute| match attribute { NeighbourAttribute::Destination(address) => neighbour_ip(address), _ => None })?;
            if host.as_deref().is_some_and(|expected| expected != ip.to_string()) { return None; }
            let iface = snapshot.names.get(&message.header.ifindex).cloned().unwrap_or_else(|| message.header.ifindex.to_string());
            if options.device.as_deref().is_some_and(|expected| expected != iface) { return None; }
            let address = message.attributes.iter().find_map(|attribute| match attribute { NeighbourAttribute::LinkLayerAddress(bytes) => Some(mac(bytes)), _ => None }).unwrap_or_else(|| "<incomplete>".into());
            Some((ip, address, iface, message.header.state.to_string()))
        }).collect::<Vec<_>>();
        if bsd {
            for (ip, address, iface, _) in entries {
                let name = if numeric { "?".into() } else { reverse_name(ip).unwrap_or_else(|| "?".into()) };
                println!("{name} ({ip}) at {address} [ether] on {iface}");
            }
        } else {
            if !entries.is_empty() { println!("Address                  HWtype  HWaddress           Flags Mask            Iface"); }
            for (ip, address, iface, state) in entries {
                let name = if numeric { ip.to_string() } else { reverse_name(ip).unwrap_or_else(|| ip.to_string()) };
                let flag = if state == "incomplete" || state == "failed" { "" } else { "C" };
                println!("{name:<24} ether   {address:<19} {flag:<5}                 {iface}");
            }
        }
        Ok(())
    })
}

fn reverse_name(address: IpAddr) -> Option<String> {
    std::net::ToSocketAddrs::to_socket_addrs(&(address, 0)).ok()?;
    None
}

fn run_ifconfig(args: Vec<OsString>) -> Result<(), String> {
    let values = utf8_args(args)?;
    let mut all = false;
    let mut short = false;
    let mut device = None;
    for value in values.into_iter().skip(1) {
        match value.as_str() {
            "-a" | "--all" => all = true,
            "-s" | "--short" => {
                short = true;
                all = true;
            }
            "-h" | "--help" => {
                println!(
                    "Usage: ifconfig [-a] [-s] [interface]\n\
                     Display network interface configuration (read-only)."
                );
                return Ok(());
            }
            value if value.starts_with('-') => return Err(format!("unsupported option '{value}'")),
            _ if device.is_none() => device = Some(value),
            _ => return Err("configuration operations are not supported".into()),
        }
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(async move {
        let (snapshot, _handle) = Snapshot::load().await?;
        let options = IpOptions {
            device,
            ..IpOptions::default()
        };
        if short {
            println!(
                "Iface             MTU    RX-OK RX-ERR RX-DRP RX-OVR    TX-OK TX-ERR TX-DRP TX-OVR Flg"
            );
        }
        for link in snapshot
            .links
            .iter()
            .filter(|link| link_matches(link, &options))
        {
            if !all
                && options.device.is_none()
                && link.header.flags.bits() & libc::IFF_UP as u32 == 0
            {
                continue;
            }
            let info = link_info(link);
            let flags = link.header.flags.bits();
            if short {
                let stats = info.stats;
                let compact_flags = format!(
                    "{}{}{}",
                    if flags & libc::IFF_UP as u32 != 0 {
                        "U"
                    } else {
                        ""
                    },
                    if flags & libc::IFF_RUNNING as u32 != 0 {
                        "R"
                    } else {
                        ""
                    },
                    if flags & libc::IFF_LOOPBACK as u32 != 0 {
                        "L"
                    } else {
                        ""
                    }
                );
                println!(
                    "{:<16} {:>6} {:>8} {:>6} {:>6} {:>6} {:>8} {:>6} {:>6} {:>6} {}",
                    info.name,
                    info.mtu.unwrap_or_default(),
                    stats.map_or(0, |stats| stats.rx_packets),
                    stats.map_or(0, |stats| stats.rx_errors),
                    stats.map_or(0, |stats| stats.rx_dropped),
                    0,
                    stats.map_or(0, |stats| stats.tx_packets),
                    stats.map_or(0, |stats| stats.tx_errors),
                    stats.map_or(0, |stats| stats.tx_dropped),
                    0,
                    compact_flags
                );
                continue;
            }
            println!(
                "{}: flags={}<{flags_text}>  mtu {}",
                info.name,
                flags,
                info.mtu.unwrap_or_default(),
                flags_text = ifconfig_flags(flags)
            );
            for address in snapshot
                .addresses
                .iter()
                .filter(|address| address.header.index == link.header.index)
                .filter_map(address_value)
            {
                if address.0.is_ipv4() {
                    let mask = prefix_mask_v4(address.1);
                    println!("        inet {}  netmask {}", address.0, mask);
                } else {
                    println!(
                        "        inet6 {}  prefixlen {}  scopeid {}",
                        address.0, address.1, address.2
                    );
                }
            }
            if let Some(address) = info.address {
                println!(
                    "        ether {address}  txqueuelen {}  ({})",
                    info.tx_queue_len.unwrap_or_default(),
                    link.header.link_layer_type
                );
            }
            if let Some(stats) = info.stats {
                println!(
                    "        RX packets {}  bytes {}",
                    stats.rx_packets, stats.rx_bytes
                );
                println!(
                    "        RX errors {}  dropped {}",
                    stats.rx_errors, stats.rx_dropped
                );
                println!(
                    "        TX packets {}  bytes {}",
                    stats.tx_packets, stats.tx_bytes
                );
                println!(
                    "        TX errors {}  dropped {}",
                    stats.tx_errors, stats.tx_dropped
                );
            }
            println!();
        }
        Ok(())
    })
}

pub fn ipcalc(args: Vec<OsString>) -> i32 {
    run("ipcalc", || run_ipcalc(args))
}

fn run_ipcalc(args: Vec<OsString>) -> Result<(), String> {
    let values = utf8_args(args)?;
    let mut network_only = false;
    let mut broadcast_only = false;
    let mut netmask_only = false;
    let mut prefix_only = false;
    let mut positional = Vec::new();
    for value in values.into_iter().skip(1) {
        match value.as_str() {
            "-n" | "--network" => network_only = true,
            "-b" | "--broadcast" => broadcast_only = true,
            "-m" | "--netmask" => netmask_only = true,
            "-p" | "--prefix" => prefix_only = true,
            "-h" | "--help" => {
                println!("Usage: ipcalc [-nmbp] ADDRESS[/PREFIX] [NETMASK]");
                return Ok(());
            }
            value if value.starts_with('-') => return Err(format!("unknown option '{value}'")),
            _ => positional.push(value),
        }
    }
    let address_spec = positional.first().ok_or("missing IP address")?;
    let (address_text, inline_prefix) = address_spec
        .split_once('/')
        .map_or((address_spec.as_str(), None), |(address, prefix)| {
            (address, Some(prefix))
        });
    let address: IpAddr = address_text
        .parse()
        .map_err(|_| format!("invalid address '{address_text}'"))?;
    let prefix = if let Some(value) = inline_prefix {
        value
            .parse::<u8>()
            .map_err(|_| format!("invalid prefix '{value}'"))?
    } else if let Some(value) = positional.get(1) {
        parse_netmask(value, address.is_ipv6())?
    } else if address.is_ipv4() {
        24
    } else {
        64
    };
    let max = if address.is_ipv4() { 32 } else { 128 };
    if prefix > max {
        return Err(format!("prefix must be between 0 and {max}"));
    }

    match address {
        IpAddr::V4(address) => {
            let raw = u32::from(address);
            let mask = if prefix == 0 {
                0
            } else {
                u32::MAX << (32 - prefix)
            };
            let network = raw & mask;
            let broadcast = network | !mask;
            if network_only {
                println!("NETWORK={}", Ipv4Addr::from(network));
            } else if broadcast_only {
                println!("BROADCAST={}", Ipv4Addr::from(broadcast));
            } else if netmask_only {
                println!("NETMASK={}", Ipv4Addr::from(mask));
            } else if prefix_only {
                println!("PREFIX={prefix}");
            } else {
                println!("Address:   {address}");
                println!("Netmask:   {} = {prefix}", Ipv4Addr::from(mask));
                println!("Wildcard:  {}", Ipv4Addr::from(!mask));
                println!("Network:   {}/{}", Ipv4Addr::from(network), prefix);
                if prefix <= 30 {
                    println!("HostMin:   {}", Ipv4Addr::from(network + 1));
                    println!("HostMax:   {}", Ipv4Addr::from(broadcast - 1));
                }
                println!("Broadcast: {}", Ipv4Addr::from(broadcast));
                let hosts = if prefix <= 30 {
                    (1u128 << (32 - prefix)) - 2
                } else {
                    1u128 << (32 - prefix)
                };
                println!("Hosts/Net: {hosts}");
            }
        }
        IpAddr::V6(address) => {
            let raw = u128::from(address);
            let mask = if prefix == 0 {
                0
            } else {
                u128::MAX << (128 - prefix)
            };
            let network = Ipv6Addr::from(raw & mask);
            if network_only {
                println!("NETWORK={network}");
            } else if netmask_only {
                println!("NETMASK={}", Ipv6Addr::from(mask));
            } else if prefix_only {
                println!("PREFIX={prefix}");
            } else if broadcast_only {
                return Err("IPv6 has no broadcast address".into());
            } else {
                println!("Address:   {address}");
                println!("Netmask:   {} = {prefix}", Ipv6Addr::from(mask));
                println!("Network:   {network}/{prefix}");
                println!(
                    "Hosts/Net: {}",
                    if prefix == 0 {
                        "2^128".into()
                    } else {
                        (1u128 << (128 - prefix).min(127)).to_string()
                    }
                );
            }
        }
    }
    Ok(())
}

fn parse_netmask(value: &str, ipv6: bool) -> Result<u8, String> {
    if let Ok(prefix) = value.parse() {
        return Ok(prefix);
    }
    if ipv6 {
        let mask: Ipv6Addr = value
            .parse()
            .map_err(|_| format!("invalid netmask '{value}'"))?;
        let raw = u128::from(mask);
        let prefix = raw.leading_ones() as u8;
        if raw
            != if prefix == 0 {
                0
            } else {
                u128::MAX << (128 - prefix)
            }
        {
            return Err("non-contiguous netmask".into());
        }
        Ok(prefix)
    } else {
        let mask: Ipv4Addr = value
            .parse()
            .map_err(|_| format!("invalid netmask '{value}'"))?;
        let raw = u32::from(mask);
        let prefix = raw.leading_ones() as u8;
        if raw
            != if prefix == 0 {
                0
            } else {
                u32::MAX << (32 - prefix)
            }
        {
            return Err("non-contiguous netmask".into());
        }
        Ok(prefix)
    }
}

fn utf8_args(args: Vec<OsString>) -> Result<Vec<String>, String> {
    args.into_iter()
        .map(|value| {
            value.into_string().map_err(|value| {
                format!("argument is not valid UTF-8: {}", value.to_string_lossy())
            })
        })
        .collect()
}

fn family_matches(actual: AddressFamily, expected: Option<AddressFamily>) -> bool {
    expected.is_none_or(|expected| expected == actual)
}

fn link_matches(link: &LinkMessage, options: &IpOptions) -> bool {
    let name = link
        .attributes
        .iter()
        .find_map(|attribute| match attribute {
            LinkAttribute::IfName(name) => Some(name.as_str()),
            _ => None,
        });
    if options
        .device
        .as_deref()
        .is_some_and(|expected| name != Some(expected))
    {
        return false;
    }
    if options.up_only && !link.header.flags.contains(LinkFlags::Up) {
        return false;
    }
    if let Some(expected) = options.link_type.as_deref()
        && link_info(link).kind.as_deref() != Some(expected)
    {
        return false;
    }
    true
}

#[derive(Default)]
struct LinkInfo {
    name: String,
    mtu: Option<u32>,
    qdisc: Option<String>,
    state: String,
    mode: String,
    group: u32,
    address: Option<String>,
    broadcast: Option<String>,
    permanent_address: Option<String>,
    alternate_names: Vec<String>,
    carrier: Option<u8>,
    kind: Option<String>,
    tx_queue_len: Option<u32>,
    stats: Option<LinkStats>,
}

#[derive(Clone, Copy)]
struct LinkStats {
    rx_packets: u64,
    tx_packets: u64,
    rx_bytes: u64,
    tx_bytes: u64,
    rx_errors: u64,
    tx_errors: u64,
    rx_dropped: u64,
    tx_dropped: u64,
}

fn link_info(link: &LinkMessage) -> LinkInfo {
    let mut info = LinkInfo {
        state: "UNKNOWN".into(),
        mode: "DEFAULT".into(),
        ..LinkInfo::default()
    };
    for attribute in &link.attributes {
        match attribute {
            LinkAttribute::IfName(value) => info.name.clone_from(value),
            LinkAttribute::Mtu(value) => info.mtu = Some(*value),
            LinkAttribute::Qdisc(value) => info.qdisc = Some(value.clone()),
            LinkAttribute::OperState(value) => info.state = value.to_string().to_uppercase(),
            LinkAttribute::Mode(value) => info.mode = value.to_string(),
            LinkAttribute::Group(value) => info.group = *value,
            LinkAttribute::Address(value) => {
                info.address = Some(link_address(value, link.header.link_layer_type))
            }
            LinkAttribute::Broadcast(value) => {
                info.broadcast = Some(link_address(value, link.header.link_layer_type))
            }
            LinkAttribute::PermAddress(value) => {
                info.permanent_address = Some(link_address(value, link.header.link_layer_type))
            }
            LinkAttribute::PropList(values) => {
                info.alternate_names
                    .extend(values.iter().filter_map(|value| match value {
                        Prop::AltIfName(name) => Some(name.clone()),
                        _ => None,
                    }));
            }
            LinkAttribute::Carrier(value) => info.carrier = Some(*value),
            LinkAttribute::LinkInfo(values) => {
                info.kind = values.iter().find_map(|value| match value {
                    LinkInfoAttribute::Kind(kind) => Some(kind.to_string()),
                    _ => None,
                });
            }
            LinkAttribute::TxQueueLen(value) => info.tx_queue_len = Some(*value),
            LinkAttribute::Stats(value) => {
                info.stats = Some(LinkStats {
                    rx_packets: value.rx_packets.into(),
                    tx_packets: value.tx_packets.into(),
                    rx_bytes: value.rx_bytes.into(),
                    tx_bytes: value.tx_bytes.into(),
                    rx_errors: value.rx_errors.into(),
                    tx_errors: value.tx_errors.into(),
                    rx_dropped: value.rx_dropped.into(),
                    tx_dropped: value.tx_dropped.into(),
                })
            }
            LinkAttribute::Stats64(value) => {
                info.stats = Some(LinkStats {
                    rx_packets: value.rx_packets,
                    tx_packets: value.tx_packets,
                    rx_bytes: value.rx_bytes,
                    tx_bytes: value.tx_bytes,
                    rx_errors: value.rx_errors,
                    tx_errors: value.tx_errors,
                    rx_dropped: value.rx_dropped,
                    tx_dropped: value.tx_dropped,
                })
            }
            _ => {}
        }
    }
    info
}

fn link_json(link: &LinkMessage) -> serde_json::Map<String, serde_json::Value> {
    let info = link_info(link);
    let flags = ip_flags(link, &info);
    let mut value = serde_json::Map::new();
    value.insert("ifindex".into(), link.header.index.into());
    value.insert("ifname".into(), info.name.into());
    value.insert("flags".into(), flags.into());
    insert_optional(&mut value, "mtu", info.mtu);
    insert_optional(&mut value, "qdisc", info.qdisc);
    value.insert("operstate".into(), info.state.into());
    value.insert("linkmode".into(), info.mode.into());
    value.insert(
        "group".into(),
        if info.group == 0 {
            "default".into()
        } else {
            info.group.to_string().into()
        },
    );
    insert_optional(&mut value, "txqlen", info.tx_queue_len);
    value.insert(
        "link_type".into(),
        link.header
            .link_layer_type
            .to_string()
            .to_ascii_lowercase()
            .into(),
    );
    insert_optional(&mut value, "address", info.address);
    insert_optional(&mut value, "broadcast", info.broadcast);
    insert_optional(&mut value, "permaddr", info.permanent_address);
    if !info.alternate_names.is_empty() {
        value.insert("altnames".into(), info.alternate_names.into());
    }
    if let Some(kind) = info.kind {
        value.insert(
            "linkinfo".into(),
            serde_json::json!({
                "info_kind": kind,
            }),
        );
    }
    value
}

fn ip_flags(link: &LinkMessage, info: &LinkInfo) -> Vec<&'static str> {
    let flags = link.header.flags;
    let mut names = Vec::new();
    if flags.contains(LinkFlags::Up) && info.carrier == Some(0) {
        names.push("NO-CARRIER");
    }
    for (flag, name) in [
        (LinkFlags::Broadcast, "BROADCAST"),
        (LinkFlags::Loopback, "LOOPBACK"),
        (LinkFlags::Pointopoint, "POINTOPOINT"),
        (LinkFlags::Multicast, "MULTICAST"),
        (LinkFlags::Noarp, "NOARP"),
        (LinkFlags::Allmulti, "ALLMULTI"),
        (LinkFlags::Promisc, "PROMISC"),
        (LinkFlags::Controller, "MASTER"),
        (LinkFlags::Port, "SLAVE"),
        (LinkFlags::Up, "UP"),
        (LinkFlags::LowerUp, "LOWER_UP"),
        (LinkFlags::Dormant, "DORMANT"),
        (LinkFlags::Echo, "ECHO"),
    ] {
        if flags.contains(flag) {
            names.push(name);
        }
    }
    names
}

fn address_json(message: &AddressMessage) -> Option<serde_json::Map<String, serde_json::Value>> {
    let address = message
        .attributes
        .iter()
        .find_map(|attribute| match attribute {
            AddressAttribute::Local(value) | AddressAttribute::Address(value) => Some(*value),
            _ => None,
        })?;
    let mut value = serde_json::Map::new();
    value.insert(
        "family".into(),
        if address.is_ipv4() { "inet" } else { "inet6" }.into(),
    );
    value.insert("local".into(), address.to_string().into());
    value.insert("prefixlen".into(), message.header.prefix_len.into());
    let scope = message.header.scope.to_string();
    value.insert(
        "scope".into(),
        if scope == "universe" {
            "global".into()
        } else {
            scope.into()
        },
    );

    let mut flags = AddressFlags::from_bits_retain(u32::from(message.header.flags.bits()));
    for attribute in &message.attributes {
        match attribute {
            AddressAttribute::Label(label) => {
                value.insert("label".into(), label.clone().into());
            }
            AddressAttribute::Broadcast(address) => {
                value.insert("broadcast".into(), address.to_string().into());
            }
            AddressAttribute::Flags(value) => flags = *value,
            AddressAttribute::CacheInfo(cache) => {
                value.insert("valid_life_time".into(), cache.ifa_valid.into());
                value.insert("preferred_life_time".into(), cache.ifa_preferred.into());
            }
            _ => {}
        }
    }
    if flags.contains(AddressFlags::Secondary) {
        value.insert(
            if address.is_ipv6() {
                "temporary"
            } else {
                "secondary"
            }
            .into(),
            true.into(),
        );
    }
    for (flag, name) in [
        (AddressFlags::Nodad, "nodad"),
        (AddressFlags::Optimistic, "optimistic"),
        (AddressFlags::Dadfailed, "dadfailed"),
        (AddressFlags::Homeaddress, "home"),
        (AddressFlags::Deprecated, "deprecated"),
        (AddressFlags::Tentative, "tentative"),
        (AddressFlags::Managetempaddr, "mngtmpaddr"),
        (AddressFlags::Noprefixroute, "noprefixroute"),
        (AddressFlags::Mcautojoin, "autojoin"),
        (AddressFlags::StablePrivacy, "stable-privacy"),
    ] {
        if !name.is_empty() && flags.contains(flag) {
            value.insert(name.into(), true.into());
        }
    }
    Some(value)
}

fn insert_optional<T>(
    value: &mut serde_json::Map<String, serde_json::Value>,
    name: &str,
    item: Option<T>,
) where
    serde_json::Value: From<T>,
{
    if let Some(item) = item {
        value.insert(name.into(), item.into());
    }
}

fn address_value(message: &AddressMessage) -> Option<(IpAddr, u8, String)> {
    let address = message
        .attributes
        .iter()
        .find_map(|attribute| match attribute {
            AddressAttribute::Local(value) | AddressAttribute::Address(value) => Some(*value),
            _ => None,
        })?;
    Some((
        address,
        message.header.prefix_len,
        scope_name(message.header.scope.to_string()),
    ))
}

/// iproute2 scope names: the kernel's `universe` prints as `global`.
fn scope_name(scope: String) -> String {
    if scope == "universe" {
        "global".into()
    } else {
        scope
    }
}

/// Everything the iproute2 `ip addr` text output needs for one address.
struct AddressText {
    address: IpAddr,
    prefix: u8,
    scope: String,
    broadcast: Option<String>,
    secondary: bool,
    deprecated: bool,
    dynamic: bool,
    managed_temporary: bool,
    noprefixroute: bool,
    label: Option<String>,
    valid_lft: Option<u32>,
    preferred_lft: Option<u32>,
}

fn address_text(message: &AddressMessage) -> Option<AddressText> {
    let address = message
        .attributes
        .iter()
        .find_map(|attribute| match attribute {
            AddressAttribute::Local(value) | AddressAttribute::Address(value) => Some(*value),
            _ => None,
        })?;
    let mut text = AddressText {
        address,
        prefix: message.header.prefix_len,
        scope: scope_name(message.header.scope.to_string()),
        broadcast: None,
        secondary: false,
        deprecated: false,
        dynamic: false,
        managed_temporary: false,
        noprefixroute: false,
        label: None,
        valid_lft: None,
        preferred_lft: None,
    };
    let mut flags = AddressFlags::from_bits_retain(u32::from(message.header.flags.bits()));
    for attribute in &message.attributes {
        match attribute {
            AddressAttribute::Broadcast(value) => {
                text.broadcast = Some(value.to_string());
            }
            AddressAttribute::Label(label) => text.label = Some(label.clone()),
            AddressAttribute::Flags(value) => flags = *value,
            AddressAttribute::CacheInfo(cache) => {
                text.valid_lft = Some(cache.ifa_valid);
                text.preferred_lft = Some(cache.ifa_preferred);
            }
            _ => {}
        }
    }
    text.secondary = flags.contains(AddressFlags::Secondary);
    text.deprecated = flags.contains(AddressFlags::Deprecated);
    text.dynamic = text.valid_lft.is_some_and(|lifetime| lifetime != u32::MAX);
    text.managed_temporary = flags.contains(AddressFlags::Managetempaddr);
    text.noprefixroute = flags.contains(AddressFlags::Noprefixroute);
    Some(text)
}

fn neighbour_ip(address: &NeighbourAddress) -> Option<IpAddr> {
    match address {
        NeighbourAddress::Inet(value) => Some((*value).into()),
        NeighbourAddress::Inet6(value) => Some((*value).into()),
        _ => None,
    }
}

fn route_ip(address: &RouteAddress) -> Option<IpAddr> {
    match address {
        RouteAddress::Inet(value) => Some((*value).into()),
        RouteAddress::Inet6(value) => Some((*value).into()),
        _ => None,
    }
}

fn link_address(bytes: &[u8], link_type: LinkLayerType) -> String {
    match (link_type, bytes) {
        (LinkLayerType::Tunnel | LinkLayerType::Sit | LinkLayerType::Ipgre, [a, b, c, d]) => {
            Ipv4Addr::new(*a, *b, *c, *d).to_string()
        }
        (LinkLayerType::Tunnel6 | LinkLayerType::Ip6gre, bytes) => {
            if let Ok(octets) = <[u8; 16]>::try_from(bytes) {
                Ipv6Addr::from(octets).to_string()
            } else {
                mac(bytes)
            }
        }
        _ => mac(bytes),
    }
}

fn mac(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

fn table_name(table: u32) -> String {
    match table {
        253 => "default".into(),
        254 => "main".into(),
        255 => "local".into(),
        _ => table.to_string(),
    }
}

fn ifconfig_flags(flags: u32) -> String {
    [
        (libc::IFF_UP, "UP"),
        (libc::IFF_BROADCAST, "BROADCAST"),
        (libc::IFF_DEBUG, "DEBUG"),
        (libc::IFF_LOOPBACK, "LOOPBACK"),
        (libc::IFF_POINTOPOINT, "POINTOPOINT"),
        (libc::IFF_RUNNING, "RUNNING"),
        (libc::IFF_NOARP, "NOARP"),
        (libc::IFF_PROMISC, "PROMISC"),
        (libc::IFF_MULTICAST, "MULTICAST"),
    ]
    .into_iter()
    .filter_map(|(mask, name)| (flags & mask as u32 != 0).then_some(name))
    .collect::<Vec<_>>()
    .join(",")
}

fn prefix_mask_v4(prefix: u8) -> Ipv4Addr {
    Ipv4Addr::from(if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_json_formats_addresses_by_link_type_and_length() {
        let ipv6 = "2001:db8::1".parse::<Ipv6Addr>().unwrap().octets();
        for (link_type, bytes, expected) in [
            (LinkLayerType::Tunnel6, vec![0; 16], "::"),
            (LinkLayerType::Tunnel6, ipv6.to_vec(), "2001:db8::1"),
            (LinkLayerType::Ip6gre, ipv6.to_vec(), "2001:db8::1"),
            (LinkLayerType::Tunnel, vec![192, 0, 2, 1], "192.0.2.1"),
            (LinkLayerType::Sit, vec![192, 0, 2, 1], "192.0.2.1"),
            (LinkLayerType::Ipgre, vec![192, 0, 2, 1], "192.0.2.1"),
            (
                LinkLayerType::Ether,
                vec![0, 1, 2, 3, 4, 5],
                "00:01:02:03:04:05",
            ),
            (LinkLayerType::Loopback, vec![0; 6], "00:00:00:00:00:00"),
            (LinkLayerType::Tunnel6, vec![0; 4], "00:00:00:00"),
            (LinkLayerType::Tunnel, vec![0; 6], "00:00:00:00:00:00"),
            (
                LinkLayerType::Ether,
                vec![0; 16],
                "00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00",
            ),
        ] {
            let mut link = LinkMessage::default();
            link.header.link_layer_type = link_type;
            link.attributes = vec![
                LinkAttribute::Address(bytes.clone()),
                LinkAttribute::Broadcast(bytes.clone()),
                LinkAttribute::PermAddress(bytes),
            ];
            let json = link_json(&link);
            for field in ["address", "broadcast", "permaddr"] {
                assert_eq!(json[field], expected, "{link_type:?} {field}");
            }
        }
    }

    #[test]
    fn ipcalc_rejects_non_contiguous_masks() {
        assert!(parse_netmask("255.0.255.0", false).is_err());
    }

    #[test]
    fn ipcalc_accepts_prefix_and_dotted_masks() {
        assert_eq!(parse_netmask("24", false).unwrap(), 24);
        assert_eq!(parse_netmask("255.255.255.0", false).unwrap(), 24);
        assert_eq!(parse_netmask("ffff:ffff:ffff:ffff::", true).unwrap(), 64);
    }
}
