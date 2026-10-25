use std::borrow::Cow;
use std::collections::HashMap;
use std::ffi::{CStr, OsString};
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};

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
    RouteAddress, RouteAttribute, RouteCacheInfo, RouteFlags, RouteMessage, RouteMetric,
    RouteRealm, RouteVia,
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
    run_output("arp", |out| run_arp(args, out))
}

pub fn ifconfig(args: Vec<OsString>) -> i32 {
    run_output("ifconfig", |out| run_ifconfig(args, out))
}

pub fn ip(args: Vec<OsString>) -> i32 {
    run_output("ip", |out| run_ip(args, None, out))
}

pub fn ipaddr(args: Vec<OsString>) -> i32 {
    run_output("ipaddr", |out| run_ip(args, Some(Object::Address), out))
}

pub fn iplink(args: Vec<OsString>) -> i32 {
    run_output("iplink", |out| run_ip(args, Some(Object::Link), out))
}

pub fn ipneigh(args: Vec<OsString>) -> i32 {
    run_output("ipneigh", |out| run_ip(args, Some(Object::Neighbour), out))
}

pub fn iproute(args: Vec<OsString>) -> i32 {
    run_output("iproute", |out| run_ip(args, Some(Object::Route), out))
}

pub fn iprule(args: Vec<OsString>) -> i32 {
    run_output("iprule", |out| run_ip(args, Some(Object::Rule), out))
}

#[derive(Debug)]
struct CommandError {
    message: String,
    code: i32,
    io_kind: Option<io::ErrorKind>,
}

impl CommandError {
    fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code,
            io_kind: None,
        }
    }
}

impl From<String> for CommandError {
    fn from(message: String) -> Self {
        Self::new(1, message)
    }
}

impl From<&str> for CommandError {
    fn from(message: &str) -> Self {
        Self::new(1, message)
    }
}

impl From<io::Error> for CommandError {
    fn from(error: io::Error) -> Self {
        Self {
            message: os_error(&error),
            code: 1,
            io_kind: Some(error.kind()),
        }
    }
}

impl From<rtnetlink::Error> for CommandError {
    fn from(error: rtnetlink::Error) -> Self {
        match error {
            rtnetlink::Error::NetlinkError(message) => {
                let code = message
                    .code
                    .map_or(libc::EIO, |code| code.get().unsigned_abs() as i32);
                Self::new(
                    2,
                    format!(
                        "RTNETLINK answers: {}",
                        os_error(&io::Error::from_raw_os_error(code))
                    ),
                )
            }
            error => Self::new(2, error.to_string()),
        }
    }
}

fn os_error(error: &io::Error) -> String {
    error.raw_os_error().map_or_else(
        || error.to_string(),
        |code| {
            // SAFETY: strerror returns a process-owned NUL-terminated string for every errno.
            unsafe { CStr::from_ptr(libc::strerror(code)) }
                .to_string_lossy()
                .into_owned()
        },
    )
}

fn run_output(
    name: &str,
    operation: impl FnOnce(&mut dyn Write) -> Result<(), CommandError>,
) -> i32 {
    let stdout = io::stdout();
    let mut out = BufWriter::new(stdout.lock());
    let result = operation(&mut out);
    // Flush already produced records even when a later input/device lookup fails.
    let flushed = out.flush().map_err(CommandError::from);
    match result.and(flushed) {
        Ok(()) => 0,
        Err(error) => {
            let mut stderr = io::stderr().lock();
            if error.io_kind.is_some() {
                let _ = writeln!(stderr, "{name}: {}", error.message);
            } else {
                let _ = writeln!(stderr, "{}", error.message);
            }
            error.code
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

const IP_USAGE: &str = concat!(
    "Usage: ip [ OPTIONS ] OBJECT { COMMAND | help }\n",
    "       ip route get ADDRESS [from ADDRESS] [mark MARK] [uid UID] [dev IFACE | oif IFACE]\n",
    "       OBJECT := { address | link | neighbour | route | rule }\n",
    "       COMMAND := { show | list } (read-only; route also supports get)\n",
    "       OPTIONS := { -4 | -6 | -f { inet | inet6 | link } | -o | -br | -s | -j | -p | -N }",
);

#[derive(Default)]
struct IpOptions {
    family: Option<AddressFamily>,
    oneline: bool,
    brief: bool,
    stats: bool,
    json: bool,
    pretty: bool,
    numeric: bool,
    device: Option<String>,
    device_index: Option<u32>,
    up_only: bool,
    link_type: Option<String>,
    master: Option<String>,
    master_index: Option<u32>,
    vrf: bool,
    no_master: bool,
    group: Option<u32>,
    scope: Option<u8>,
    address_prefix: Option<IpNet>,
    address_protocol: Option<u8>,
    address_flags: Vec<(AddressFlags, bool)>,
    label: Option<std::ffi::CString>,
    /// Routes default to main; rules default to all tables. Zero selects all.
    route_table: Option<u32>,
    route_protocol: Option<u8>,
    route_prefix: RoutePrefixes,
    route_kind: Option<u8>,
    neighbour_states: Option<u16>,
    neighbour_prefix: Option<IpNet>,
}

#[derive(Debug)]
struct RouteGet {
    destination: IpAddr,
    source: Option<IpAddr>,
    mark: Option<u32>,
    uid: Option<u32>,
}

struct IpRequest {
    object: Object,
    options: IpOptions,
    query: Option<RouteGet>,
    output: Option<IpOutput>,
}

#[derive(Clone, Copy)]
enum IpOutput {
    Help,
    Version,
}

impl IpRequest {
    fn display(output: IpOutput) -> Self {
        Self {
            object: Object::Address,
            options: IpOptions::default(),
            query: None,
            output: Some(output),
        }
    }
}

fn run_ip(
    args: Vec<OsString>,
    forced: Option<Object>,
    out: &mut dyn Write,
) -> Result<(), CommandError> {
    let args = utf8_args(args)?;
    let request = parse_ip(&args[1..], forced)?;
    match request.output {
        Some(IpOutput::Help) => return Err(CommandError::new(0, IP_USAGE)),
        Some(IpOutput::Version) => {
            writeln!(out, "ip (axe) {}", env!("CARGO_PKG_VERSION"))?;
            return Ok(());
        }
        None => {}
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let (connection, handle, _) = rtnetlink::new_connection()?;
        let connection = tokio::spawn(connection);
        let result = async {
            let mut request = request;
            let mut snapshot = Snapshot::load_links(&handle).await?;
            resolve_selectors(&snapshot, &mut request.options, request.object)?;
            if let Some(query) = request.query {
                let message = route_get_request(&request.options, &query);
                let mut routes = handle.route().get(message).execute();
                let route = routes.try_next().await?.ok_or_else(|| {
                    CommandError::new(2, "RTNETLINK answers: Network is unreachable")
                })?;
                let route = RouteView::new(route);
                return render_routes(
                    &snapshot,
                    std::iter::once(&route),
                    &request.options,
                    Some(&query),
                    out,
                );
            }
            snapshot
                .load_object(&handle, request.object, &request.options)
                .await?;
            render_ip(&snapshot, request.object, &request.options, out)
        }
        .await;
        connection.abort();
        result
    })
}

fn parse_ip(args: &[String], forced: Option<Object>) -> Result<IpRequest, CommandError> {
    let mut options = IpOptions::default();
    let mut values = args.iter().map(String::as_str);
    let mut first = None;
    while let Some(value) = values.next() {
        match value {
            "-4" => options.family = Some(AddressFamily::Inet),
            "-6" => options.family = Some(AddressFamily::Inet6),
            "-0" => options.family = Some(AddressFamily::Packet),
            "-o" | "-oneline" | "--oneline" => options.oneline = true,
            "-br" | "-brief" | "--brief" => options.brief = true,
            "-s" | "-st" | "-stats" | "-statistics" | "--stats" | "--statistics" => {
                options.stats = true
            }
            "-j" | "-json" | "--json" => options.json = true,
            "-p" | "-pretty" | "--pretty" => options.pretty = true,
            "-N" | "-Numeric" | "--Numeric" => options.numeric = true,
            "-h" | "-help" | "--help" => return Ok(IpRequest::display(IpOutput::Help)),
            "-V" | "-Version" | "--version" => return Ok(IpRequest::display(IpOutput::Version)),
            "-f" | "-family" | "--family" => {
                let family = argument(&mut values, value)?;
                options.family = Some(match family {
                    "inet" | "ipv4" => AddressFamily::Inet,
                    "inet6" | "ipv6" => AddressFamily::Inet6,
                    "link" => AddressFamily::Packet,
                    _ => {
                        return Err(CommandError::new(
                            255,
                            format!(
                                "Error: argument \"{family}\" is wrong: invalid protocol family"
                            ),
                        ));
                    }
                });
            }
            value if value.starts_with('-') => {
                return Err(CommandError::new(
                    255,
                    format!("Option \"{value}\" is unknown or unsupported, try \"ip -help\"."),
                ));
            }
            value => {
                first = Some(value);
                break;
            }
        }
    }
    if first == Some("help") {
        return Ok(IpRequest::display(IpOutput::Help));
    }
    let object = if let Some(object) = forced {
        object
    } else if let Some(value) = first.take() {
        parse_object(value)?
    } else {
        return Err(CommandError::new(255, IP_USAGE));
    };
    if !matches!(object, Object::Link | Object::Address)
        && options.family == Some(AddressFamily::Packet)
    {
        return Err(CommandError::new(
            255,
            "Error: link family is only supported for link/address display",
        ));
    }
    if options.brief && !matches!(object, Object::Link | Object::Address) {
        return Err(CommandError::new(
            255,
            "Error: brief output is only supported for link/address display",
        ));
    }
    let mut positional = first.into_iter().chain(values);
    let verb = positional.next();
    let operation = verb.and_then(|verb| verb_name(object, verb));
    if operation == Some("help") {
        return Ok(IpRequest::display(IpOutput::Help));
    }
    if operation.is_some_and(|verb| !matches!(verb, "show" | "list" | "lst" | "get")) {
        return Err(CommandError::new(
            1,
            "only show/list operations and route get are supported (read-only)",
        ));
    }
    let mut selectors = verb
        .filter(|_| operation.is_none())
        .into_iter()
        .chain(positional);
    let query = if operation == Some("get") {
        if object != Object::Route {
            return Err(CommandError::new(
                1,
                "only show/list operations and route get are supported (read-only)",
            ));
        }
        let destination = argument(&mut selectors, "route get")?;
        let destination = if destination == "to" {
            argument(&mut selectors, "to")?
        } else {
            destination
        };
        let destination = parse_shorthand_address(destination)?;
        check_family(destination, options.family)?;
        let mut query = RouteGet {
            destination,
            source: None,
            mark: None,
            uid: None,
        };
        while let Some(selector) = selectors.next() {
            let value = argument(&mut selectors, selector)?;
            match selector {
                "from" => {
                    let family = Some(ip_family(destination));
                    let source = parse_shorthand_address(value)
                        .map_err(|_| address_error("prefix", value, family))?;
                    check_family(source, family)
                        .map_err(|_| address_error("prefix", value, family))?;
                    query.source = Some(source);
                }
                "mark" => {
                    query.mark = Some(parse_number(value).ok_or_else(|| invalid("mark", value))?)
                }
                "uid" => {
                    query.uid = Some(parse_number(value).ok_or_else(|| invalid("UID", value))?)
                }
                "dev" | "oif" => options.device = Some(value.into()),
                _ => return Err(invalid("route get selector", selector)),
            }
        }
        Some(query)
    } else {
        parse_show_selectors(object, &mut selectors, &mut options)?;
        None
    };
    Ok(IpRequest {
        object,
        options,
        query,
        output: None,
    })
}

fn verb_name(object: Object, value: &str) -> Option<&'static str> {
    let names: &[&str] = match object {
        Object::Link => &[
            "add", "set", "change", "replace", "delete", "show", "list", "xstats", "afstats",
            "help",
        ],
        Object::Address => &[
            "add", "change", "replace", "delete", "show", "list", "flush", "save", "restore",
            "help",
        ],
        Object::Neighbour => &[
            "add", "change", "replace", "delete", "show", "list", "flush", "get", "help",
        ],
        Object::Route => &[
            "add", "change", "chg", "replace", "prepend", "append", "test", "delete", "list",
            "show", "lst", "get", "flush", "save", "restore", "showdump", "help",
        ],
        Object::Rule => &[
            "add", "delete", "list", "show", "flush", "save", "restore", "help",
        ],
    };
    names
        .iter()
        .copied()
        .find(|name| !value.is_empty() && name.starts_with(value))
}

fn argument<'a>(
    values: &mut impl Iterator<Item = &'a str>,
    _keyword: &str,
) -> Result<&'a str, CommandError> {
    values
        .next()
        .ok_or_else(|| CommandError::new(255, "Command line is not complete. Try option \"help\""))
}

fn invalid(kind: &str, value: &str) -> CommandError {
    CommandError::new(
        255,
        format!("Error: argument \"{value}\" is wrong: invalid {kind}"),
    )
}

fn parse_show_selectors<'a>(
    object: Object,
    values: &mut impl Iterator<Item = &'a str>,
    options: &mut IpOptions,
) -> Result<(), CommandError> {
    while let Some(selector) = values.next() {
        if selector.is_empty() {
            return Err(invalid("selector", selector));
        }
        if selector.starts_with('-') {
            if object == Object::Address
                && let Some(flag) = selector.strip_prefix('-').and_then(address_flag)
            {
                options
                    .address_flags
                    .push((flag, matches!(selector, "-dynamic" | "-primary")));
                continue;
            }
            return Err(invalid("selector", selector));
        }
        match (object, selector) {
            (Object::Link | Object::Address | Object::Route | Object::Neighbour, "dev") => {
                options.device = Some(argument(values, selector)?.into());
            }
            (Object::Link | Object::Address, "up") => options.up_only = true,
            (Object::Link | Object::Address, "type") => {
                options.link_type = Some(argument(values, selector)?.into())
            }
            (Object::Link | Object::Address | Object::Neighbour, "master" | "vrf") => {
                options.master = Some(argument(values, selector)?.into());
                options.vrf = selector == "vrf";
            }
            (Object::Link | Object::Address, "nomaster") => options.no_master = true,
            (Object::Link | Object::Address, "group") => {
                let value = argument(values, selector)?;
                options.group = Some(if value == "default" {
                    0
                } else {
                    parse_number(value).ok_or_else(|| invalid("group", value))?
                });
            }
            (Object::Address | Object::Route, "scope") => {
                let value = argument(values, selector)?;
                options.scope = if value == "all" {
                    None
                } else {
                    Some(
                        parse_name(value, SCOPES)
                            .and_then(|value| u8::try_from(value).ok())
                            .ok_or_else(|| invalid("scope", value))?,
                    )
                };
            }
            (Object::Address, "to") => {
                options.address_prefix =
                    Some(parse_prefix(argument(values, selector)?, options.family)?)
            }
            (Object::Address, "label") => {
                let value = argument(values, selector)?;
                options.label =
                    Some(std::ffi::CString::new(value).map_err(|_| invalid("label", value))?);
            }
            (Object::Address, "proto") => {
                let value = argument(values, selector)?;
                options.address_protocol = Some(
                    parse_name(value, ADDRESS_PROTOCOLS)
                        .and_then(|value| u8::try_from(value).ok())
                        .ok_or_else(|| invalid("address protocol", value))?,
                );
            }
            (Object::Address, "dynamic") => {
                options.address_flags.push((AddressFlags::Permanent, false))
            }
            (Object::Address, "permanent") => {
                options.address_flags.push((AddressFlags::Permanent, true))
            }
            (Object::Address, "primary") => {
                options.address_flags.push((AddressFlags::Secondary, false))
            }
            (Object::Address, name) if address_flag(name).is_some() => {
                options
                    .address_flags
                    .push((address_flag(name).expect("matched address flag"), true));
            }
            (Object::Route | Object::Rule, "table") | (Object::Rule, "lookup") => {
                let value = argument(values, selector)?;
                options.route_table = Some(if value == "all" {
                    0
                } else {
                    parse_name(value, TABLES).ok_or_else(|| {
                        CommandError::new(
                            255,
                            format!(
                                "Error: argument \"{value}\" is wrong: table id value is invalid\n"
                            ),
                        )
                    })?
                });
            }
            (Object::Route, "proto" | "protocol") => {
                let value = argument(values, selector)?;
                options.route_protocol = Some(
                    parse_name(value, ROUTE_PROTOCOLS)
                        .and_then(|value| u8::try_from(value).ok())
                        .ok_or_else(|| invalid("protocol", value))?,
                );
            }
            (Object::Route, "type") => {
                let value = argument(values, selector)?;
                options.route_kind = Some(
                    parse_name(value, ROUTE_TYPES)
                        .and_then(|value| u8::try_from(value).ok())
                        .ok_or_else(|| invalid("route type", value))?,
                );
            }
            (Object::Route, "root" | "match" | "exact" | "to") => {
                let mut mode = match selector {
                    "root" => PrefixMode::Root,
                    "match" => PrefixMode::Match,
                    _ => PrefixMode::Exact,
                };
                let mut value = argument(values, selector)?;
                if matches!(value, "root" | "match" | "exact") {
                    mode = match value {
                        "root" => PrefixMode::Root,
                        "match" => PrefixMode::Match,
                        _ => PrefixMode::Exact,
                    };
                    value = argument(values, value)?;
                }
                options
                    .route_prefix
                    .set(parse_prefix(value, options.family)?, mode);
            }
            (Object::Route, value) => {
                options
                    .route_prefix
                    .set(parse_prefix(value, options.family)?, PrefixMode::Exact);
            }
            (Object::Neighbour, "nud") => {
                let value = argument(values, selector)?;
                let mask = if value == "all" {
                    0x1ff
                } else {
                    let state = parse_name(&value.to_ascii_lowercase(), NEIGHBOUR_STATES)
                        .filter(|state| *state <= 255)
                        .ok_or_else(|| invalid("NUD state", value))?;
                    if state == 0 { 0x100 } else { state as u16 }
                };
                *options.neighbour_states.get_or_insert(0) |= mask;
            }
            (Object::Neighbour, "to") => {
                options.neighbour_prefix =
                    Some(parse_prefix(argument(values, selector)?, options.family)?)
            }
            (Object::Neighbour, value) => {
                options.neighbour_prefix = Some(parse_prefix(value, options.family)?)
            }
            (Object::Link | Object::Address, value) if options.device.is_none() => {
                options.device = Some(value.into())
            }
            (Object::Rule, value) => {
                parse_prefix(value, Some(options.family.unwrap_or(AddressFamily::Inet)))?;
                return Err(CommandError::new(
                    255,
                    format!("Error: unsupported rule selector \"{value}\""),
                ));
            }
            _ => return Err(invalid("selector", selector)),
        }
    }
    Ok(())
}

fn parse_object(value: &str) -> Result<Object, CommandError> {
    [
        ("address", Object::Address),
        ("link", Object::Link),
        ("neighbour", Object::Neighbour),
        ("neighbor", Object::Neighbour),
        ("route", Object::Route),
        ("rule", Object::Rule),
    ]
    .into_iter()
    .find_map(|(name, object)| (!value.is_empty() && name.starts_with(value)).then_some(object))
    .ok_or_else(|| {
        CommandError::new(
            1,
            format!("Object \"{value}\" is unknown, try \"ip help\"."),
        )
    })
}

fn parse_number(value: &str) -> Option<u32> {
    if value.is_empty() || value.starts_with(['-', '+']) {
        return None;
    }
    let (digits, radix) = if let Some(value) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        (value, 16)
    } else if value.len() > 1 && value.starts_with('0') {
        (&value[1..], 8)
    } else {
        (value, 10)
    };
    u32::from_str_radix(digits, radix).ok()
}

fn parse_shorthand_address(value: &str) -> Result<IpAddr, CommandError> {
    if let Ok(address) = value.parse::<Ipv6Addr>() {
        return Ok(address.into());
    }
    let mut octets = [0; 4];
    for (count, part) in value.split('.').enumerate() {
        if count == 4 {
            return Err(address_error("address", value, None));
        }
        octets[count] = parse_number(part)
            .and_then(|part| u8::try_from(part).ok())
            .ok_or_else(|| address_error("address", value, None))?;
    }
    Ok(Ipv4Addr::from(octets).into())
}

fn ip_family(address: IpAddr) -> AddressFamily {
    if address.is_ipv4() {
        AddressFamily::Inet
    } else {
        AddressFamily::Inet6
    }
}

fn check_family(address: IpAddr, family: Option<AddressFamily>) -> Result<(), CommandError> {
    if family.is_some_and(|family| family != ip_family(address)) {
        Err(address_error("address", &address.to_string(), family))
    } else {
        Ok(())
    }
}

fn address_error(kind: &str, value: &str, family: Option<AddressFamily>) -> CommandError {
    let family = match family {
        Some(AddressFamily::Inet) => "inet",
        Some(AddressFamily::Inet6) => "inet6",
        _ => "any valid",
    };
    CommandError::new(
        1,
        format!("Error: {family} {kind} is expected rather than \"{value}\"."),
    )
}

#[derive(Clone, Copy, Debug)]
struct IpNet(IpAddr, u8);

#[derive(Clone, Copy)]
enum PrefixMode {
    Exact,
    Root,
    Match,
}

#[derive(Clone, Copy, Default)]
struct RoutePrefixes {
    root: Option<IpNet>,
    matched: Option<IpNet>,
}

impl RoutePrefixes {
    fn set(&mut self, network: IpNet, mode: PrefixMode) {
        match mode {
            PrefixMode::Exact => {
                self.root = Some(network);
                self.matched = Some(network);
            }
            PrefixMode::Root => self.root = Some(network),
            PrefixMode::Match => self.matched = Some(network),
        }
    }

    fn matches(self, route: IpNet) -> bool {
        self.root
            .is_none_or(|root| route.1 >= root.1 && prefix_matches(route.0, root.0, root.1))
            && self.matched.is_none_or(|matched| {
                route.1 <= matched.1 && prefix_matches(route.0, matched.0, route.1)
            })
    }
}

fn parse_prefix(value: &str, family: Option<AddressFamily>) -> Result<IpNet, CommandError> {
    if matches!(value, "default" | "all" | "any") {
        return Ok(IpNet(
            if family == Some(AddressFamily::Inet6) {
                Ipv6Addr::UNSPECIFIED.into()
            } else {
                Ipv4Addr::UNSPECIFIED.into()
            },
            0,
        ));
    }
    let (address, length) = value
        .split_once('/')
        .map_or((value, None), |(address, length)| (address, Some(length)));
    let address =
        parse_shorthand_address(address).map_err(|_| address_error("prefix", value, family))?;
    check_family(address, family).map_err(|_| address_error("prefix", value, family))?;
    let width = if address.is_ipv4() { 32 } else { 128 };
    let length = length
        .map_or(Some(width), |length| length.parse::<u8>().ok())
        .filter(|length| *length <= width)
        .ok_or_else(|| address_error("prefix", value, family))?;
    Ok(IpNet(address, length))
}

#[derive(Default)]
struct Snapshot {
    links: Vec<LinkView>,
    addresses: Vec<AddressView>,
    neighbours: Vec<NeighbourView>,
    routes: Vec<RouteView>,
    rules: Vec<RuleMessage>,
    indices: HashMap<u32, usize>,
}

impl Snapshot {
    async fn load_links(handle: &rtnetlink::Handle) -> Result<Self, CommandError> {
        let mut snapshot = Self::default();
        let mut stream = handle.link().get().execute();
        while let Some(link) = stream.try_next().await? {
            let link = LinkView::new(link);
            snapshot.indices.insert(link.index, snapshot.links.len());
            snapshot.links.push(link);
        }
        Ok(snapshot)
    }

    async fn load_object(
        &mut self,
        handle: &rtnetlink::Handle,
        object: Object,
        options: &IpOptions,
    ) -> Result<(), CommandError> {
        match object {
            Object::Link => {}
            Object::Address if options.family == Some(AddressFamily::Packet) => {}
            Object::Address => {
                let mut stream = handle.address().get().execute();
                while let Some(address) = stream.try_next().await? {
                    if let Some(address) = AddressView::new(address) {
                        self.addresses.push(address);
                    }
                }
            }
            Object::Neighbour => {
                let mut stream = handle.neighbours().get().execute();
                while let Some(neighbour) = stream.try_next().await? {
                    if let Some(neighbour) = NeighbourView::new(neighbour) {
                        self.neighbours.push(neighbour);
                    }
                }
            }
            Object::Route => {
                for family in [AddressFamily::Inet, AddressFamily::Inet6] {
                    if !family_matches(family, route_family(options)) {
                        continue;
                    }
                    let mut message = RouteMessage::default();
                    message.header.address_family = family;
                    let mut stream = handle.route().get(message).execute();
                    while let Some(route) = stream.try_next().await? {
                        self.routes.push(RouteView::new(route));
                    }
                }
            }
            Object::Rule => {
                let version = if options.family == Some(AddressFamily::Inet6) {
                    IpVersion::V6
                } else {
                    IpVersion::V4
                };
                let mut stream = handle.rule().get(version).execute();
                while let Some(rule) = stream.try_next().await? {
                    self.rules.push(rule);
                }
            }
        }
        Ok(())
    }

    fn link(&self, index: u32) -> Option<&LinkView> {
        self.indices
            .get(&index)
            .map(|position| &self.links[*position])
    }

    fn name(&self, index: u32) -> Cow<'_, str> {
        self.link(index).map_or_else(
            || Cow::Owned(format!("if{index}")),
            |link| Cow::Borrowed(link.name.as_str()),
        )
    }

    fn require_device(&self, name: &str) -> Option<&LinkView> {
        self.links.iter().find(|link| {
            link.name == name
                || link
                    .alternate_names
                    .iter()
                    .any(|alternate| alternate == name)
        })
    }
}

fn resolve_selectors(
    snapshot: &Snapshot,
    options: &mut IpOptions,
    object: Object,
) -> Result<(), CommandError> {
    if let Some(name) = options.device.as_deref() {
        options.device_index = Some(
            snapshot
                .require_device(name)
                .ok_or_else(|| {
                    CommandError::new(
                        1,
                        if matches!(object, Object::Link | Object::Address) {
                            format!("Device \"{name}\" does not exist.")
                        } else {
                            format!("Cannot find device \"{name}\"")
                        },
                    )
                })?
                .index,
        );
    }
    if let Some(name) = options.master.as_deref() {
        let master = if options.vrf {
            snapshot
                .require_device(name)
                .filter(|link| link.kind.as_deref() == Some("vrf"))
                .ok_or_else(|| invalid("VRF name", name))?
        } else {
            snapshot
                .require_device(name)
                .ok_or_else(|| CommandError::new(1, format!("Device \"{name}\" does not exist.")))?
        };
        options.master_index = Some(master.index);
    }
    Ok(())
}

fn render_ip(
    snapshot: &Snapshot,
    object: Object,
    options: &IpOptions,
    out: &mut dyn Write,
) -> Result<(), CommandError> {
    match object {
        Object::Link | Object::Address => render_links(snapshot, object, options, out),
        Object::Neighbour => render_neighbours(snapshot, options, out),
        Object::Route => render_routes(
            snapshot,
            snapshot
                .routes
                .iter()
                .filter(|route| route.matches(options)),
            options,
            None,
            out,
        ),
        Object::Rule => show_rules(snapshot, options, out),
    }
}

fn write_json(
    out: &mut dyn Write,
    values: impl IntoIterator<Item = serde_json::Value>,
    pretty: bool,
) -> Result<(), CommandError> {
    out.write_all(b"[")?;
    let mut first = true;
    for value in values {
        if !first {
            out.write_all(b",")?;
        }
        if pretty {
            out.write_all(b"\n")?;
        }
        first = false;
        let result = if pretty {
            serde_json::to_writer_pretty(&mut *out, &value)
        } else {
            serde_json::to_writer(&mut *out, &value)
        };
        result.map_err(|error| CommandError {
            io_kind: error.io_error_kind(),
            message: error.to_string(),
            code: 1,
        })?;
    }
    if pretty && !first {
        out.write_all(b"\n")?;
    }
    out.write_all(b"]\n")?;
    Ok(())
}

fn link_matches(link: &LinkView, options: &IpOptions) -> bool {
    options.device_index.is_none_or(|index| index == link.index)
        && (!options.up_only || link.flags.contains(LinkFlags::Up))
        && options
            .link_type
            .as_deref()
            .is_none_or(|kind| link.kind.as_deref() == Some(kind))
        && options
            .master_index
            .is_none_or(|index| link.master == Some(index))
        && (!options.no_master || link.master.is_none())
        && options.group.is_none_or(|group| link.group == group)
}

fn render_links(
    snapshot: &Snapshot,
    object: Object,
    options: &IpOptions,
    out: &mut dyn Write,
) -> Result<(), CommandError> {
    let address_mode = object == Object::Address && options.family != Some(AddressFamily::Packet);
    let selected_addresses = |index| {
        snapshot
            .addresses
            .iter()
            .filter(move |address| address.index == index && address.matches(options))
    };
    let selected = snapshot.links.iter().filter(|link| {
        if !link_matches(link, options) {
            return false;
        }
        if !address_mode || selected_addresses(link.index).next().is_some() {
            return true;
        }
        options.family.is_none()
            && options.address_prefix.is_none()
            && options.address_protocol.is_none()
            && options.address_flags.is_empty()
            && options.label.is_none()
            && !snapshot
                .addresses
                .iter()
                .any(|address| address.index == link.index)
    });
    if options.json {
        return write_json(
            out,
            selected.map(|link| {
                let mut value = link.json(snapshot, options, object);
                if address_mode {
                    value.insert(
                        "addr_info".into(),
                        selected_addresses(link.index)
                            .map(|address| serde_json::Value::Object(address.json(options)))
                            .collect::<Vec<_>>()
                            .into(),
                    );
                }
                serde_json::Value::Object(value)
            }),
            options.pretty,
        );
    }
    for link in selected {
        if options.brief {
            write!(
                out,
                "{:<16} {:<14} ",
                link.display_name(snapshot),
                link.state
            )?;
            if address_mode {
                for address in selected_addresses(link.index) {
                    address.write_brief(out)?;
                }
                writeln!(out)?;
            } else {
                if let Some(address) = &link.address {
                    write!(out, "{address} ")?;
                }
                writeln!(out, "<{}> ", link.ip_flags(snapshot).join(","))?;
            }
            continue;
        }
        if !options.oneline || !address_mode {
            render_link_header(snapshot, link, object, options, out)?;
        }
        if address_mode {
            for address in selected_addresses(link.index) {
                if options.oneline {
                    write!(out, "{}: {}    ", link.index, link.name)?;
                } else {
                    out.write_all(b"    ")?;
                }
                address.write_text(out, options)?;
            }
        }
        if options.stats && !options.oneline {
            link.write_stats(out)?;
        }
    }
    Ok(())
}

fn render_link_header(
    snapshot: &Snapshot,
    link: &LinkView,
    object: Object,
    options: &IpOptions,
    out: &mut dyn Write,
) -> Result<(), CommandError> {
    let full_link = object != Object::Address || options.family != Some(AddressFamily::Inet6);
    write!(
        out,
        "{}: {}: <{}> mtu {}",
        link.index,
        link.display_name(snapshot),
        link.ip_flags(snapshot).join(","),
        link.mtu.unwrap_or_default()
    )?;
    if full_link {
        if let Some(qdisc) = &link.qdisc {
            write!(out, " qdisc {qdisc}")?;
        }
        if let Some(master) = link.master {
            write!(out, " master {}", snapshot.name(master))?;
        }
    }
    write!(out, " state {}", link.state)?;
    if object == Object::Link {
        write!(out, " mode {}", link.mode.to_ascii_uppercase())?;
    }
    if full_link {
        write!(
            out,
            " group {}",
            name_of(link.group, GROUPS, options.numeric)
        )?;
        if let Some(queue) = link.tx_queue_len {
            write!(out, " qlen {queue}")?;
        }
    } else {
        out.write_all(b" ")?;
    }
    let separator = if options.oneline { "\\" } else { "\n" };
    if options.family.is_none() || options.family == Some(AddressFamily::Packet) {
        write!(
            out,
            "{separator}    link/{} ",
            link_type_name(link.link_type, options.numeric)
        )?;
        if let Some(address) = &link.address {
            write!(out, "{address}")?;
        }
        if let Some(broadcast) = &link.broadcast {
            write!(
                out,
                " {} {broadcast}",
                if link.flags.contains(LinkFlags::Pointopoint) {
                    "peer"
                } else {
                    "brd"
                }
            )?;
        }
        if let Some(permanent) = &link.permanent_address
            && link.address.as_deref() != Some(permanent)
        {
            write!(out, " permaddr {permanent}")?;
        }
    }
    if let Some(netns) = link.netns {
        if netns < 0 {
            out.write_all(b" link-netnsid unknown")?;
        } else {
            write!(out, " link-netnsid {netns}")?;
        }
    }
    for alternate in &link.alternate_names {
        write!(out, "{separator}    altname {alternate}")?;
    }
    writeln!(out)?;
    Ok(())
}

fn format_lifetime(seconds: u32) -> String {
    if seconds == u32::MAX {
        "forever".into()
    } else {
        format!("{seconds}sec")
    }
}

struct NeighbourView {
    destination: IpAddr,
    index: u32,
    lladdr: Option<String>,
    state: u16,
    flags: NeighbourFlags,
    probes: Option<u32>,
}

impl NeighbourView {
    fn new(message: NeighbourMessage) -> Option<Self> {
        let mut destination = None;
        let mut lladdr = None;
        let mut probes = None;
        for attribute in message.attributes {
            match attribute {
                NeighbourAttribute::Destination(address) => destination = neighbour_ip(&address),
                NeighbourAttribute::LinkLayerAddress(bytes) => lladdr = Some(mac(&bytes)),
                NeighbourAttribute::Probes(value) => probes = Some(value),
                _ => {}
            }
        }
        Some(Self {
            destination: destination?,
            index: message.header.ifindex,
            lladdr,
            state: message.header.state.into(),
            flags: message.header.flags,
            probes,
        })
    }

    fn matches(&self, snapshot: &Snapshot, options: &IpOptions) -> bool {
        let state_mask = if self.state == 0 { 0x100 } else { self.state };
        family_matches(ip_family(self.destination), options.family)
            && state_mask & options.neighbour_states.unwrap_or(0xff & !0x40) != 0
            && options.device_index.is_none_or(|index| self.index == index)
            && options.master_index.is_none_or(|index| {
                snapshot
                    .link(self.index)
                    .is_some_and(|link| link.master == Some(index))
            })
            && options
                .neighbour_prefix
                .is_none_or(|prefix| prefix_matches(self.destination, prefix.0, prefix.1))
    }

    fn states(&self) -> Vec<String> {
        NEIGHBOUR_STATES
            .iter()
            .filter(|(mask, _)| *mask != 0 && self.state & *mask as u16 != 0)
            .map(|(_, name)| name.to_ascii_uppercase())
            .collect()
    }

    fn json(&self, snapshot: &Snapshot, options: &IpOptions) -> serde_json::Value {
        let mut value = serde_json::Map::new();
        value.insert("dst".into(), self.destination.to_string().into());
        if options.device_index.is_none() {
            value.insert("dev".into(), snapshot.name(self.index).as_ref().into());
        }
        insert_optional(&mut value, "lladdr", self.lladdr.as_deref());
        if self.flags.contains(NeighbourFlags::Router) {
            value.insert("router".into(), true.into());
        }
        if self.flags.contains(NeighbourFlags::Proxy) {
            value.insert("proxy".into(), true.into());
        }
        value.insert("state".into(), self.states().into());
        if options.stats {
            insert_optional(&mut value, "probes", self.probes);
        }
        value.into()
    }
}

fn render_neighbours(
    snapshot: &Snapshot,
    options: &IpOptions,
    out: &mut dyn Write,
) -> Result<(), CommandError> {
    let selected = snapshot
        .neighbours
        .iter()
        .filter(|neighbour| neighbour.matches(snapshot, options));
    if options.json {
        return write_json(
            out,
            selected.map(|neighbour| neighbour.json(snapshot, options)),
            options.pretty,
        );
    }
    for neighbour in selected {
        write!(out, "{}", neighbour.destination)?;
        if options.device_index.is_none() {
            write!(out, " dev {}", snapshot.name(neighbour.index))?;
        }
        if let Some(address) = &neighbour.lladdr {
            write!(out, " lladdr {address}")?;
        }
        if neighbour.flags.contains(NeighbourFlags::Router) {
            out.write_all(b" router")?;
        }
        if neighbour.flags.contains(NeighbourFlags::Proxy) {
            out.write_all(b" proxy")?;
        }
        if options.stats
            && let Some(probes) = neighbour.probes
        {
            write!(out, " probes {probes}")?;
        }
        let states = neighbour.states();
        if !states.is_empty() {
            write!(out, " {}", states.join(","))?;
        }
        writeln!(out, " ")?;
    }
    Ok(())
}

fn route_family(options: &IpOptions) -> Option<AddressFamily> {
    options.family.or_else(|| {
        options
            .route_prefix
            .root
            .or(options.route_prefix.matched)
            .map(|network| ip_family(network.0))
            .or(if options.route_table == Some(0) {
                None
            } else {
                Some(AddressFamily::Inet)
            })
    })
}

struct NextHopView {
    gateway: Option<IpAddr>,
    via: Option<IpAddr>,
    index: u32,
    weight: u16,
    flags: u32,
    realm: Option<RouteRealm>,
}

struct MetricView {
    id: u8,
    name: &'static str,
    value: u32,
    time: bool,
}

struct RouteView {
    network: IpNet,
    from: Option<IpNet>,
    gateway: Option<IpAddr>,
    via: Option<IpAddr>,
    index: Option<u32>,
    source: Option<IpAddr>,
    table: u32,
    protocol: u8,
    scope: u8,
    kind: u8,
    tos: u8,
    flags: u32,
    metric: Option<u32>,
    metrics: Vec<MetricView>,
    locks: u32,
    multipath: Vec<NextHopView>,
    preference: Option<u8>,
    expires: Option<i32>,
    realm: Option<RouteRealm>,
    nhid: Option<u32>,
    uid: Option<u32>,
    mark: Option<u32>,
    cache: Option<RouteCacheInfo>,
}

impl RouteView {
    fn new(message: RouteMessage) -> Self {
        let address = if message.header.address_family == AddressFamily::Inet6 {
            Ipv6Addr::UNSPECIFIED.into()
        } else {
            Ipv4Addr::UNSPECIFIED.into()
        };
        let mut route = Self {
            network: IpNet(address, message.header.destination_prefix_length),
            from: None,
            gateway: None,
            via: None,
            index: None,
            source: None,
            table: u32::from(message.header.table),
            protocol: message.header.protocol.into(),
            scope: message.header.scope.into(),
            kind: message.header.kind.into(),
            tos: message.header.tos,
            flags: message.header.flags.bits(),
            metric: None,
            metrics: Vec::new(),
            locks: 0,
            multipath: Vec::new(),
            preference: None,
            expires: None,
            realm: None,
            nhid: None,
            uid: None,
            mark: None,
            cache: None,
        };
        for attribute in message.attributes {
            match attribute {
                RouteAttribute::Destination(value) => {
                    if let Some(value) = route_ip(&value) {
                        route.network.0 = value;
                    }
                }
                RouteAttribute::Source(value) => {
                    route.from = route_ip(&value)
                        .map(|value| IpNet(value, message.header.source_prefix_length))
                }
                RouteAttribute::Gateway(value) => route.gateway = route_ip(&value),
                RouteAttribute::Via(value) => route.via = via_ip(&value),
                RouteAttribute::PrefSource(value) => route.source = route_ip(&value),
                RouteAttribute::Oif(index) => route.index = Some(index),
                RouteAttribute::Priority(value) => route.metric = Some(value),
                RouteAttribute::Table(value) => route.table = value,
                RouteAttribute::Preference(value) => route.preference = Some(value.into()),
                RouteAttribute::Expires(value) => route.expires = Some((value / user_hz()) as i32),
                RouteAttribute::Realm(value) => route.realm = Some(value),
                RouteAttribute::NhId(value) => route.nhid = Some(value),
                RouteAttribute::Uid(value) => route.uid = Some(value),
                RouteAttribute::Mark(value) => route.mark = Some(value),
                RouteAttribute::CacheInfo(value) => {
                    if value.expires != 0 {
                        route.expires = Some((value.expires as i32) / user_hz() as i32);
                    }
                    route.cache = Some(value);
                }
                RouteAttribute::Metrics(values) => {
                    for metric in values {
                        let (id, name, value, divisor, time) = match metric {
                            RouteMetric::Lock(value) => {
                                route.locks = value;
                                continue;
                            }
                            RouteMetric::Mtu(value) => (2, "mtu", value, 1, false),
                            RouteMetric::Window(value) => (3, "window", value, 1, false),
                            RouteMetric::Rtt(value) => (4, "rtt", value, 8, true),
                            RouteMetric::RttVar(value) => (5, "rttvar", value, 4, true),
                            RouteMetric::SsThresh(value) => (6, "ssthresh", value, 1, false),
                            RouteMetric::Cwnd(value) => (7, "cwnd", value, 1, false),
                            RouteMetric::Advmss(value) => (8, "advmss", value, 1, false),
                            RouteMetric::Reordering(value) => (9, "reordering", value, 1, false),
                            RouteMetric::Hoplimit(u32::MAX) => continue,
                            RouteMetric::Hoplimit(value) => (10, "hoplimit", value, 1, false),
                            RouteMetric::InitCwnd(value) => (11, "initcwnd", value, 1, false),
                            RouteMetric::Features(value) => (12, "features", value, 1, false),
                            RouteMetric::RtoMin(value) => (13, "rto_min", value, 1, true),
                            RouteMetric::InitRwnd(value) => (14, "initrwnd", value, 1, false),
                            RouteMetric::QuickAck(value) => (15, "quickack", value, 1, false),
                            RouteMetric::FastopenNoCookie(value) => {
                                (17, "fastopen_no_cookie", value, 1, false)
                            }
                            _ => continue,
                        };
                        route.metrics.push(MetricView {
                            id,
                            name,
                            value: value / divisor,
                            time,
                        });
                    }
                    route.metrics.sort_unstable_by_key(|metric| metric.id);
                }
                RouteAttribute::MultiPath(values) => {
                    for next in values {
                        let mut hop = NextHopView {
                            gateway: None,
                            via: None,
                            index: next.interface_index,
                            weight: u16::from(next.hops) + 1,
                            flags: u32::from(next.flags.bits()),
                            realm: None,
                        };
                        for attribute in next.attributes {
                            match attribute {
                                RouteAttribute::Gateway(value) => hop.gateway = route_ip(&value),
                                RouteAttribute::Via(value) => hop.via = via_ip(&value),
                                RouteAttribute::Realm(value) => hop.realm = Some(value),
                                _ => {}
                            }
                        }
                        route.multipath.push(hop);
                    }
                }
                _ => {}
            }
        }
        route
    }

    fn matches(&self, options: &IpOptions) -> bool {
        let table = options.route_table.unwrap_or(254);
        family_matches(ip_family(self.network.0), route_family(options))
            && self.flags & RouteFlags::Cloned.bits() == 0
            && (table == 0 || self.table == table)
            && options
                .route_protocol
                .is_none_or(|protocol| self.protocol == protocol)
            && options.route_kind.is_none_or(|kind| self.kind == kind)
            && options.scope.is_none_or(|scope| self.scope == scope)
            && options.device_index.is_none_or(|index| {
                self.index == Some(index) || self.multipath.iter().any(|hop| hop.index == index)
            })
            && options.route_prefix.matches(self.network)
    }

    fn show_table(&self, options: &IpOptions) -> bool {
        self.table != 0 && self.table != 254 && options.route_table.is_none_or(|table| table == 0)
    }

    fn show_protocol(&self, options: &IpOptions) -> bool {
        self.flags & RouteFlags::Cloned.bits() == 0
            && self.protocol != 3
            && options.route_protocol.is_none()
    }

    fn show_scope(&self, options: &IpOptions) -> bool {
        self.flags & RouteFlags::Cloned.bits() == 0 && self.scope != 0 && options.scope.is_none()
    }

    fn json(
        &self,
        snapshot: &Snapshot,
        options: &IpOptions,
        query: Option<&RouteGet>,
    ) -> serde_json::Value {
        let mut value = serde_json::Map::new();
        if self.kind != 1 {
            value.insert(
                "type".into(),
                name_of(u32::from(self.kind), ROUTE_TYPES, options.numeric)
                    .as_ref()
                    .into(),
            );
        }
        value.insert("dst".into(), network_text(self.network).into());
        if let Some(from) = self.from {
            value.insert("from".into(), network_text(from).into());
        } else if let Some(source) = query.and_then(|query| query.source) {
            value.insert("from".into(), source.to_string().into());
        }
        insert_optional(&mut value, "nhid", self.nhid);
        if self.tos != 0 {
            value.insert(
                "tos".into(),
                dsfield_name(self.tos, options.numeric).as_ref().into(),
            );
        }
        insert_optional(
            &mut value,
            "gateway",
            self.gateway.map(|address| address.to_string()),
        );
        if let Some(via) = self.via {
            value.insert(
                "via".into(),
                serde_json::json!({"family": family_name(via), "host": via.to_string()}),
            );
        }
        if let Some(index) = self.index
            && (options.device_index.is_none() || query.is_some())
        {
            value.insert("dev".into(), snapshot.name(index).as_ref().into());
        }
        if self.show_table(options) {
            value.insert(
                "table".into(),
                name_of(self.table, TABLES, options.numeric).as_ref().into(),
            );
        }
        if self.show_protocol(options) {
            value.insert(
                "protocol".into(),
                name_of(u32::from(self.protocol), ROUTE_PROTOCOLS, options.numeric)
                    .as_ref()
                    .into(),
            );
        }
        if self.show_scope(options) {
            value.insert(
                "scope".into(),
                name_of(u32::from(self.scope), SCOPES, options.numeric)
                    .as_ref()
                    .into(),
            );
        }
        insert_optional(
            &mut value,
            "prefsrc",
            self.source.map(|address| address.to_string()),
        );
        insert_optional(&mut value, "metric", self.metric);
        value.insert(
            "flags".into(),
            route_flags(self.flags).collect::<Vec<_>>().into(),
        );
        insert_optional(&mut value, "mark", self.mark.filter(|mark| *mark != 0));
        if let Some(realm) = self.realm {
            value.insert("flow".into(), realm_json(realm, options.numeric));
        }
        insert_optional(&mut value, "uid", self.uid);
        if self.flags & RouteFlags::Cloned.bits() != 0 && self.network.0.is_ipv4() {
            value.insert(
                "cache".into(),
                cache_flags(self.flags).collect::<Vec<_>>().into(),
            );
        }
        insert_optional(&mut value, "expires", self.expires);
        if let Some(cache) = self.cache {
            if cache.error != 0 {
                value.insert("error".into(), cache.error.into());
            }
            if options.stats {
                if cache.clntref != 0 {
                    value.insert("users".into(), cache.clntref.into());
                }
                if cache.used != 0 {
                    value.insert("used".into(), cache.used.into());
                }
                if cache.last_use != 0 {
                    value.insert("age".into(), (cache.last_use / user_hz()).into());
                }
            }
        }
        if !self.metrics.is_empty() {
            let metrics: serde_json::Map<_, _> = self
                .metrics
                .iter()
                .map(|metric| (metric.name.into(), metric.value.into()))
                .collect();
            value.insert("metrics".into(), serde_json::json!([metrics]));
        }
        if let Some(preference) = self.preference {
            value.insert(
                "pref".into(),
                name_of(u32::from(preference), PREFERENCES, false)
                    .as_ref()
                    .into(),
            );
        }
        if !self.multipath.is_empty() {
            value.insert("nexthops".into(), self.multipath.iter().map(|hop| {
                let mut next = serde_json::Map::new();
                insert_optional(&mut next, "gateway", hop.gateway.map(|address| address.to_string()));
                if let Some(via) = hop.via { next.insert("via".into(), serde_json::json!({"family": family_name(via), "host": via.to_string()})); }
                next.insert("dev".into(), snapshot.name(hop.index).as_ref().into());
                next.insert("weight".into(), hop.weight.into());
                next.insert("flags".into(), route_flags(hop.flags).collect::<Vec<_>>().into());
                if let Some(realm) = hop.realm { next.insert("flow".into(), realm_json(realm, options.numeric)); }
                serde_json::Value::Object(next)
            }).collect::<Vec<_>>().into());
        }
        value.into()
    }

    fn write_text(
        &self,
        snapshot: &Snapshot,
        options: &IpOptions,
        query: Option<&RouteGet>,
        out: &mut dyn Write,
    ) -> Result<(), CommandError> {
        if self.kind != 1 {
            write!(
                out,
                "{} ",
                name_of(u32::from(self.kind), ROUTE_TYPES, options.numeric)
            )?;
        }
        write!(out, "{} ", network_text(self.network))?;
        if let Some(from) = self.from {
            write!(out, "from {} ", network_text(from))?;
        } else if let Some(source) = query.and_then(|query| query.source) {
            write!(out, "from {source} ")?;
        }
        if let Some(nhid) = self.nhid {
            write!(out, "nhid {nhid} ")?;
        }
        if self.tos != 0 {
            write!(out, "tos {} ", dsfield_name(self.tos, options.numeric))?;
        }
        if let Some(gateway) = self.gateway {
            write!(out, "via {gateway} ")?;
        }
        if let Some(via) = self.via {
            write!(out, "via {} {via} ", family_name(via))?;
        }
        if let Some(index) = self.index
            && (options.device_index.is_none() || query.is_some())
        {
            write!(out, "dev {} ", snapshot.name(index))?;
        }
        if self.show_table(options) {
            write!(
                out,
                "table {} ",
                name_of(self.table, TABLES, options.numeric)
            )?;
        }
        if self.show_protocol(options) {
            write!(
                out,
                "proto {} ",
                name_of(u32::from(self.protocol), ROUTE_PROTOCOLS, options.numeric)
            )?;
        }
        if self.show_scope(options) {
            write!(
                out,
                "scope {} ",
                name_of(u32::from(self.scope), SCOPES, options.numeric)
            )?;
        }
        if let Some(source) = self.source {
            write!(out, "src {source} ")?;
        }
        if let Some(metric) = self.metric {
            write!(out, "metric {metric} ")?;
        }
        for flag in route_flags(self.flags) {
            write!(out, "{flag} ")?;
        }
        if let Some(mark) = self.mark.filter(|mark| *mark != 0) {
            if mark >= 16 {
                write!(out, "mark 0x{mark:x} ")?;
            } else {
                write!(out, "mark {mark} ")?;
            }
        }
        if let Some(realm) = self.realm {
            write_realm(out, realm, options.numeric)?;
        }
        if let Some(uid) = self.uid {
            write!(out, "uid {uid} ")?;
        }
        let separator = if options.oneline { "\\" } else { "\n" };
        if self.flags & RouteFlags::Cloned.bits() != 0 && self.network.0.is_ipv4() {
            write!(out, "{separator}    cache ")?;
            let flags = cache_flags(self.flags).collect::<Vec<_>>();
            if !flags.is_empty() {
                write!(out, "<{}> ", flags.join(","))?;
            }
        }
        if let Some(expires) = self.expires {
            write!(out, "expires {expires}sec ")?;
        }
        if let Some(cache) = self.cache {
            if cache.error != 0 {
                write!(out, "error {} ", cache.error)?;
            }
            if options.stats {
                if cache.clntref != 0 {
                    write!(out, "users {} ", cache.clntref)?;
                }
                if cache.used != 0 {
                    write!(out, "used {} ", cache.used)?;
                }
                if cache.last_use != 0 {
                    write!(out, "age {}sec ", cache.last_use / user_hz())?;
                }
            }
        }
        for metric in &self.metrics {
            write!(out, "{} ", metric.name)?;
            if self.locks & (1 << metric.id) != 0 {
                out.write_all(b"lock ")?;
            }
            if metric.time {
                if metric.value >= 1000 {
                    write!(out, "{}s ", f64::from(metric.value) / 1000.0)?;
                } else {
                    write!(out, "{}ms ", metric.value)?;
                }
            } else {
                write!(out, "{} ", metric.value)?;
            }
        }
        if let Some(preference) = self.preference {
            write!(
                out,
                "pref {}",
                name_of(u32::from(preference), PREFERENCES, false)
            )?;
        }
        for next in &self.multipath {
            write!(out, "{separator}\tnexthop ")?;
            if let Some(gateway) = next.gateway {
                write!(out, "via {gateway} ")?;
            }
            if let Some(via) = next.via {
                write!(out, "via {} {via} ", family_name(via))?;
            }
            if let Some(realm) = next.realm {
                write_realm(out, realm, options.numeric)?;
            }
            write!(
                out,
                "dev {} weight {} ",
                snapshot.name(next.index),
                next.weight
            )?;
            for flag in route_flags(next.flags) {
                write!(out, "{flag} ")?;
            }
        }
        writeln!(out)?;
        Ok(())
    }
}

fn render_routes<'a>(
    snapshot: &Snapshot,
    routes: impl Iterator<Item = &'a RouteView>,
    options: &IpOptions,
    query: Option<&RouteGet>,
    out: &mut dyn Write,
) -> Result<(), CommandError> {
    if options.json {
        return write_json(
            out,
            routes.map(|route| route.json(snapshot, options, query)),
            options.pretty,
        );
    }
    for route in routes {
        route.write_text(snapshot, options, query, out)?;
    }
    Ok(())
}

fn route_get_request(options: &IpOptions, query: &RouteGet) -> RouteMessage {
    let mut message = RouteMessage::default();
    message.header.address_family = ip_family(query.destination);
    message.header.destination_prefix_length = if query.destination.is_ipv4() { 32 } else { 128 };
    if query.destination.is_ipv4() {
        message.header.flags = RouteFlags::LookupTable;
    }
    message
        .attributes
        .push(RouteAttribute::Destination(route_address(
            query.destination,
        )));
    if let Some(source) = query.source {
        message.header.source_prefix_length = if source.is_ipv4() { 32 } else { 128 };
        message
            .attributes
            .push(RouteAttribute::Source(route_address(source)));
    }
    if let Some(mark) = query.mark {
        message.attributes.push(RouteAttribute::Mark(mark));
    }
    if let Some(uid) = query.uid {
        message.attributes.push(RouteAttribute::Uid(uid));
    }
    if let Some(index) = options.device_index {
        message.attributes.push(RouteAttribute::Oif(index));
    }
    message
}

fn route_address(address: IpAddr) -> RouteAddress {
    match address {
        IpAddr::V4(address) => RouteAddress::Inet(address),
        IpAddr::V6(address) => RouteAddress::Inet6(address),
    }
}

fn via_ip(address: &RouteVia) -> Option<IpAddr> {
    match address {
        RouteVia::Inet(address) => Some((*address).into()),
        RouteVia::Inet6(address) => Some((*address).into()),
        _ => None,
    }
}

fn family_name(address: IpAddr) -> &'static str {
    if address.is_ipv4() { "inet" } else { "inet6" }
}

fn network_text(network: IpNet) -> String {
    if network.1 == 0 && network.0.is_unspecified() {
        "default".into()
    } else if network.1 == if network.0.is_ipv4() { 32 } else { 128 } {
        network.0.to_string()
    } else {
        format!("{}/{}", network.0, network.1)
    }
}

fn route_flags(flags: u32) -> impl Iterator<Item = &'static str> {
    ROUTE_FLAGS
        .iter()
        .filter_map(move |(mask, name)| (flags & mask != 0).then_some(*name))
}

fn cache_flags(flags: u32) -> impl Iterator<Item = &'static str> {
    CACHE_FLAGS
        .iter()
        .filter_map(move |(mask, name)| (flags & mask != 0).then_some(*name))
}

fn realm_json(realm: RouteRealm, numeric: bool) -> serde_json::Value {
    let mut value = serde_json::Map::new();
    if realm.source != 0 {
        value.insert(
            "from".into(),
            name_of(u32::from(realm.source), REALMS, numeric)
                .as_ref()
                .into(),
        );
    }
    value.insert(
        "to".into(),
        name_of(u32::from(realm.destination), REALMS, numeric)
            .as_ref()
            .into(),
    );
    value.into()
}

fn write_realm(out: &mut dyn Write, realm: RouteRealm, numeric: bool) -> io::Result<()> {
    if realm.source == 0 {
        write!(
            out,
            "realm {} ",
            name_of(u32::from(realm.destination), REALMS, numeric)
        )
    } else {
        write!(
            out,
            "realms {}/{} ",
            name_of(u32::from(realm.source), REALMS, numeric),
            name_of(u32::from(realm.destination), REALMS, numeric)
        )
    }
}

fn user_hz() -> u32 {
    static HZ: std::sync::LazyLock<u32> = std::sync::LazyLock::new(|| {
        // SAFETY: sysconf(_SC_CLK_TCK) has no pointer arguments or side effects.
        let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        u32::try_from(hz).ok().filter(|hz| *hz != 0).unwrap_or(100)
    });
    *HZ
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

fn show_rules(
    snapshot: &Snapshot,
    options: &IpOptions,
    out: &mut dyn Write,
) -> Result<(), CommandError> {
    let rules = snapshot
        .rules
        .iter()
        .map(|rule| {
            let table = rule
                .attributes
                .iter()
                .find_map(|attribute| match attribute {
                    RuleAttribute::Table(value) => Some(*value),
                    _ => None,
                })
                .unwrap_or(u32::from(rule.header.table));
            (rule, table)
        })
        .filter(|(_, table)| {
            options
                .route_table
                .is_none_or(|selected| selected == 0 || selected == *table)
        })
        .map(|(rule, table)| {
            let mut priority = 0;
            let mut source = "all".to_string();
            let mut destination = None;
            let mut iif = None;
            let mut oif = None;
            let mut fwmark = None;
            for attribute in &rule.attributes {
                match attribute {
                    RuleAttribute::Priority(value) => priority = *value,
                    RuleAttribute::Source(value) => {
                        source = network_text(IpNet(*value, rule.header.src_len))
                    }
                    RuleAttribute::Destination(value) => {
                        destination = Some(network_text(IpNet(*value, rule.header.dst_len)))
                    }
                    RuleAttribute::Iifname(value) => iif = Some(value.as_str()),
                    RuleAttribute::Oifname(value) => oif = Some(value.as_str()),
                    RuleAttribute::FwMark(value) => fwmark = Some(*value),
                    _ => {}
                }
            }
            (priority, source, destination, table, iif, oif, fwmark)
        });
    if options.json {
        write_json(
            out,
            rules.map(|(priority, source, destination, table, iif, oif, fwmark)| {
                let mut value = serde_json::Map::new();
                value.insert("priority".into(), priority.into());
                value.insert("src".into(), source.into());
                value.insert(
                    "table".into(),
                    name_of(table, TABLES, options.numeric).as_ref().into(),
                );
                insert_optional(&mut value, "dst", destination);
                insert_optional(&mut value, "iif", iif);
                insert_optional(&mut value, "oif", oif);
                insert_optional(&mut value, "fwmark", fwmark);
                value.into()
            }),
            options.pretty,
        )?;
    } else {
        for (priority, source, destination, table, iif, oif, fwmark) in rules {
            write!(out, "{priority}:\tfrom {source}")?;
            if let Some(destination) = destination {
                write!(out, " to {destination}")?;
            }
            if let Some(mark) = fwmark {
                write!(out, " fwmark 0x{mark:x}")?;
            }
            if let Some(iif) = iif {
                write!(out, " iif {iif}")?;
            }
            if let Some(oif) = oif {
                write!(out, " oif {oif}")?;
            }
            writeln!(out, " lookup {}", name_of(table, TABLES, options.numeric))?;
        }
    }
    Ok(())
}

#[derive(Default)]
struct ArpOptions {
    numeric: bool,
    all: bool,
    linux: bool,
    verbose: bool,
    device: Option<String>,
    host: Option<String>,
    hardware: Option<u32>,
    help: bool,
}

fn parse_arp(args: &[String]) -> Result<ArpOptions, CommandError> {
    let mut options = ArpOptions::default();
    let mut values = args.iter().map(String::as_str);
    let mut operands = false;
    while let Some(value) = values.next() {
        if value == "--" && !operands {
            operands = true;
            continue;
        }
        if !operands && value.starts_with("--") {
            let (name, attached) = value
                .split_once('=')
                .map_or((value, None), |(name, value)| (name, Some(value)));
            let flag = match name {
                "--numeric" => 'n',
                "--all" => 'a',
                "--verbose" => 'v',
                "--device" => 'i',
                "--hw-type" => 'H',
                "--protocol" => 'A',
                "--help" => 'h',
                _ => {
                    return Err(CommandError::new(
                        3,
                        format!(
                            "arp: unrecognized or unsupported option '{name}' (display mode only)"
                        ),
                    ));
                }
            };
            if matches!(flag, 'i' | 'H' | 'A') {
                let argument = attached.or_else(|| values.next()).ok_or_else(|| {
                    CommandError::new(3, format!("arp: option '{name}' requires an argument"))
                })?;
                arp_option_argument(&mut options, flag, argument)?;
            } else {
                if attached.is_some() {
                    return Err(CommandError::new(
                        3,
                        format!("arp: option '{name}' doesn't allow an argument"),
                    ));
                }
                arp_option_flag(&mut options, flag)?;
            }
        } else if !operands && value.starts_with('-') && value != "-" {
            for (offset, flag) in value[1..].char_indices() {
                if matches!(flag, 'i' | 'H' | 't' | 'A' | 'p') {
                    let attached = &value[1 + offset + flag.len_utf8()..];
                    let argument = if attached.is_empty() {
                        values.next().ok_or_else(|| {
                            CommandError::new(
                                3,
                                format!("arp: option requires an argument -- '{flag}'"),
                            )
                        })?
                    } else {
                        attached
                    };
                    arp_option_argument(&mut options, flag, argument)?;
                    break;
                }
                arp_option_flag(&mut options, flag)?;
            }
        } else if options.host.is_none() {
            options.host = Some(value.into());
        } else {
            return Err(CommandError::new(3, "arp: too many arguments"));
        }
    }
    Ok(options)
}

fn arp_option_flag(options: &mut ArpOptions, flag: char) -> Result<(), CommandError> {
    match flag {
        'n' => options.numeric = true,
        'a' => options.all = true,
        'e' => options.linux = true,
        'v' => options.verbose = true,
        'h' | '?' => options.help = true,
        'd' | 's' | 'f' | 'D' => {
            return Err(CommandError::new(
                3,
                "arp: configuration operations are not supported (display mode only)",
            ));
        }
        _ => {
            return Err(CommandError::new(
                3,
                format!("arp: invalid option -- '{flag}'"),
            ));
        }
    }
    Ok(())
}

fn arp_option_argument(
    options: &mut ArpOptions,
    flag: char,
    value: &str,
) -> Result<(), CommandError> {
    match flag {
        'i' => options.device = (!value.is_empty()).then(|| value.into()),
        'H' | 't' => {
            options.hardware = Some(parse_name(value, LINK_TYPES).ok_or_else(|| {
                CommandError::new(255, format!("arp: {value}: unknown hardware type."))
            })?)
        }
        'A' | 'p' if value == "inet" => {}
        'A' | 'p' => {
            return Err(CommandError::new(
                255,
                format!("arp: {value}: kernel only supports 'inet'."),
            ));
        }
        _ => {
            return Err(CommandError::new(
                3,
                format!("arp: invalid option -- '{flag}'"),
            ));
        }
    }
    Ok(())
}

fn run_arp(args: Vec<OsString>, out: &mut dyn Write) -> Result<(), CommandError> {
    let values = utf8_args(args)?;
    let options = parse_arp(&values[1..])?;
    if options.help {
        writeln!(
            out,
            "Usage: arp [-vnea] [-H type] [-i interface] [hostname]\nDisplay the kernel ARP cache (read-only)."
        )?;
        return Ok(());
    }
    let host = options.host.as_deref().map(resolve_arp_host).transpose()?;
    let file = std::fs::File::open("/proc/net/arp")
        .map_err(|error| CommandError::new(255, format!("/proc/net/arp: {}", os_error(&error))))?;
    render_arp(BufReader::new(file), &options, host, out)
}

fn resolve_arp_host(host: &str) -> Result<Ipv4Addr, CommandError> {
    if let Some(address) = legacy_ipv4(host) {
        return Ok(address);
    }
    (host, 0)
        .to_socket_addrs()
        .ok()
        .and_then(|mut addresses| {
            addresses.find_map(|address| match address.ip() {
                IpAddr::V4(address) => Some(address),
                _ => None,
            })
        })
        .ok_or_else(|| CommandError::new(255, format!("{host}: Host name lookup failure")))
}

fn legacy_ipv4(value: &str) -> Option<Ipv4Addr> {
    let mut parts = value.split('.').peekable();
    let mut address = 0u32;
    let mut count = 0;
    while let Some(part) = parts.next() {
        let part = parse_number(part)?;
        if count == 4 {
            return None;
        }
        if parts.peek().is_some() {
            if part > 255 || count == 3 {
                return None;
            }
            address |= part << (24 - count * 8);
        } else {
            let available = 32 - count * 8;
            if available < 32 && part >= (1u32 << available) {
                return None;
            }
            address |= part;
        }
        count += 1;
    }
    (count != 0).then(|| Ipv4Addr::from(address))
}

struct ArpRow<'a> {
    address: Ipv4Addr,
    hardware: u32,
    flags: u32,
    lladdr: &'a str,
    mask: &'a str,
    device: &'a str,
}

fn arp_row(line: &str) -> Option<ArpRow<'_>> {
    let mut fields = line.split_whitespace();
    let address = fields.next()?.parse().ok()?;
    let hardware = parse_number(fields.next()?)?;
    let flags = parse_number(fields.next()?)?;
    let first = fields.next()?;
    let second = fields.next()?;
    let third = fields.next();
    let (lladdr, mask, device) = if let Some(third) = third {
        (first, second, third)
    } else {
        ("", first, second)
    };
    Some(ArpRow {
        address,
        hardware,
        flags,
        lladdr,
        mask,
        device,
    })
}

fn render_arp(
    mut input: impl BufRead,
    options: &ArpOptions,
    host: Option<Ipv4Addr>,
    out: &mut dyn Write,
) -> Result<(), CommandError> {
    let mut line = String::new();
    input.read_line(&mut line)?;
    let mut entries = 0;
    let mut shown = 0;
    loop {
        line.clear();
        if input.read_line(&mut line)? == 0 {
            break;
        }
        let row = arp_row(&line)
            .ok_or_else(|| CommandError::new(255, "/proc/net/arp: invalid ARP record"))?;
        entries += 1;
        if host.is_some_and(|host| row.address != host)
            || options
                .hardware
                .is_some_and(|hardware| row.hardware != hardware)
            || options
                .device
                .as_deref()
                .is_some_and(|device| row.device != device)
        {
            continue;
        }
        let name = if options.numeric {
            None
        } else {
            reverse_name(row.address)
        };
        if !options.all || options.linux {
            if shown == 0 {
                writeln!(
                    out,
                    "Address                  HWtype  HWaddress           Flags Mask            Iface"
                )?;
            }
            let name = name.unwrap_or_else(|| row.address.to_string());
            let mut flags = String::new();
            for (mask, flag) in [
                (2, 'C'),
                (4, 'M'),
                (8, 'P'),
                (256, 'A'),
                (512, '!'),
                (16, 'T'),
            ] {
                if row.flags & mask != 0 {
                    flags.push(flag);
                }
            }
            let complete = row.flags & 2 != 0;
            let hardware = if complete {
                link_type_name(row.hardware as u16, false)
            } else if row.flags & 8 != 0 {
                Cow::Borrowed("*")
            } else {
                Cow::Borrowed("")
            };
            let lladdr = if complete {
                row.lladdr
            } else if row.flags & 8 != 0 {
                "<from_interface>"
            } else {
                "(incomplete)"
            };
            let mask = if row.flags & 32 != 0 { row.mask } else { "" };
            writeln!(
                out,
                "{name:<23.23}  {hardware:<8.8}{lladdr:<20.20}{flags:<6.6}{mask:<15.15} {}",
                row.device
            )?;
        } else {
            write!(
                out,
                "{} ({}) at ",
                name.as_deref().unwrap_or("?"),
                row.address
            )?;
            if row.flags & 2 != 0 {
                write!(
                    out,
                    "{} [{}] ",
                    row.lladdr,
                    link_type_name(row.hardware as u16, false)
                )?;
            } else if row.flags & 8 != 0 {
                out.write_all(b"<from_interface> ")?;
            } else {
                out.write_all(b"<incomplete> ")?;
            }
            if row.flags & 32 != 0 {
                write!(out, "netmask {} ", row.mask)?;
            }
            for (mask, flag) in [
                (4, "PERM"),
                (8, "PUB"),
                (256, "AUTO"),
                (512, "DONTPUB"),
                (16, "TRAIL"),
            ] {
                if row.flags & mask != 0 {
                    write!(out, "{flag} ")?;
                }
            }
            writeln!(out, "on {}", row.device)?;
        }
        shown += 1;
    }
    if options.verbose {
        writeln!(
            out,
            "Entries: {entries}\tSkipped: {}\tFound: {shown}",
            entries - shown
        )?;
    }
    if shown == 0 {
        if let (Some(name), Some(host)) = (options.host.as_deref(), host)
            && !options.all
        {
            writeln!(out, "{name} ({host}) -- no entry")?;
        } else if options.hardware.is_some() || options.host.is_some() || options.device.is_some() {
            writeln!(out, "arp: in {entries} entries no match found.")?;
        }
    }
    Ok(())
}

fn reverse_name(address: Ipv4Addr) -> Option<String> {
    let socket = libc::sockaddr_in {
        sin_family: libc::AF_INET as libc::sa_family_t,
        sin_port: 0,
        sin_addr: libc::in_addr {
            s_addr: u32::from_ne_bytes(address.octets()),
        },
        sin_zero: [0; 8],
    };
    let mut host = [0 as libc::c_char; libc::NI_MAXHOST as usize];
    // SAFETY: sockaddr and output pointers have their declared sizes and remain alive for the call.
    let status = unsafe {
        libc::getnameinfo(
            (&socket as *const libc::sockaddr_in).cast(),
            std::mem::size_of_val(&socket) as libc::socklen_t,
            host.as_mut_ptr(),
            host.len() as libc::socklen_t,
            std::ptr::null_mut(),
            0,
            libc::NI_NAMEREQD,
        )
    };
    if status != 0 {
        return None;
    }
    // SAFETY: successful getnameinfo writes a NUL-terminated host string.
    Some(
        unsafe { CStr::from_ptr(host.as_ptr()) }
            .to_string_lossy()
            .into_owned(),
    )
}

fn run_ifconfig(args: Vec<OsString>, out: &mut dyn Write) -> Result<(), CommandError> {
    let values = utf8_args(args)?;
    let mut all = false;
    let mut short = false;
    let mut device = None;
    for value in &values[1..] {
        match value.as_str() {
            "-a" | "--all" => all = true,
            "-s" | "--short" => short = true,
            "-v" | "--verbose" => {}
            "-h" | "--help" => {
                writeln!(
                    out,
                    "Usage: ifconfig [-a] [-s] [-v] [interface]\nDisplay network interface configuration (read-only)."
                )?;
                return Ok(());
            }
            value if value.starts_with('-') => {
                for flag in value[1..].chars() {
                    match flag {
                        'a' => all = true,
                        's' => short = true,
                        'v' => {}
                        _ => {
                            return Err(CommandError::new(
                                1,
                                format!("ifconfig: invalid option -- '{flag}'"),
                            ));
                        }
                    }
                }
            }
            _ if device.is_none() => device = Some(value.as_str()),
            _ => {
                return Err(CommandError::new(
                    1,
                    "ifconfig: configuration operations are not supported",
                ));
            }
        }
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let (connection, handle, _) = rtnetlink::new_connection()?;
        let connection = tokio::spawn(connection);
        let result = async {
            let mut snapshot = Snapshot::load_links(&handle).await?;
            let index = ifconfig_device(&snapshot, device, short, out)?;
            if !short {
                snapshot
                    .load_object(&handle, Object::Address, &IpOptions::default())
                    .await?;
                if let Ok(input) = std::fs::File::open("/proc/net/if_inet6") {
                    order_ifconfig_ipv6(BufReader::new(input), &mut snapshot.addresses)?;
                }
            }
            render_ifconfig(&snapshot, all, short, index, out)
        }
        .await;
        connection.abort();
        result
    })
}

fn ifconfig_device(
    snapshot: &Snapshot,
    device: Option<&str>,
    short: bool,
    out: &mut dyn Write,
) -> Result<Option<u32>, CommandError> {
    if short {
        writeln!(
            out,
            "Iface      MTU    RX-OK RX-ERR RX-DRP RX-OVR    TX-OK TX-ERR TX-DRP TX-OVR Flg"
        )?;
    }
    device
        .map(|name| {
            snapshot
                .require_device(name)
                .map(|link| link.index)
                .ok_or_else(|| {
                    CommandError::new(
                        1,
                        format!("{name}: error fetching interface information: Device not found"),
                    )
                })
        })
        .transpose()
}

fn render_ifconfig(
    snapshot: &Snapshot,
    all: bool,
    short: bool,
    device: Option<u32>,
    out: &mut dyn Write,
) -> Result<(), CommandError> {
    let mut links = snapshot
        .links
        .iter()
        .filter(|link| {
            device.map_or(all || link.flags.contains(LinkFlags::Up), |index| {
                link.index == index
            })
        })
        .collect::<Vec<_>>();
    links.sort_unstable_by(|left, right| ifconfig_name_cmp(&left.name, &right.name));
    for link in links {
        let flags = link.flags.bits() & 0xffff;
        if short {
            let compact_flags = [
                (libc::IFF_ALLMULTI, 'A'),
                (libc::IFF_BROADCAST, 'B'),
                (libc::IFF_DEBUG, 'D'),
                (libc::IFF_LOOPBACK, 'L'),
                (libc::IFF_MULTICAST, 'M'),
                (libc::IFF_DYNAMIC, 'd'),
                (libc::IFF_PROMISC, 'P'),
                (libc::IFF_NOTRAILERS, 'N'),
                (libc::IFF_NOARP, 'O'),
                (libc::IFF_POINTOPOINT, 'P'),
                (libc::IFF_SLAVE, 's'),
                (libc::IFF_MASTER, 'm'),
                (libc::IFF_RUNNING, 'R'),
                (libc::IFF_UP, 'U'),
            ]
            .into_iter()
            .filter_map(|(mask, flag)| (flags & mask as u32 != 0).then_some(flag));
            write!(
                out,
                "{:<15.15} {:>5} ",
                link.name,
                link.mtu.unwrap_or_default()
            )?;
            if let Some(stats) = link.stats {
                write!(
                    out,
                    "{:>8} {:>6} {:>6} {:<6} {:>8} {:>6} {:>6} {:>6} ",
                    stats.rx_packets,
                    stats.rx_errors,
                    stats.rx_dropped.wrapping_add(stats.rx_missed_errors),
                    stats.rx_fifo_errors,
                    stats.tx_packets,
                    stats.tx_errors,
                    stats.tx_dropped,
                    stats.tx_fifo_errors
                )?;
            } else {
                write!(out, "{:<56}", "     - no statistics available -")?;
            }
            if flags == 0 {
                out.write_all(b"[NO FLAGS]")?;
            } else {
                for flag in compact_flags {
                    write!(out, "{flag}")?;
                }
            }
            writeln!(out)?;
            continue;
        }
        writeln!(
            out,
            "{}: flags={}<{}>  mtu {}",
            link.name,
            flags & 0xffff,
            ifconfig_flags(flags),
            link.mtu.unwrap_or_default()
        )?;
        for address in snapshot
            .addresses
            .iter()
            .filter(|address| address.index == link.index)
        {
            if address.address.is_ipv4() {
                write!(
                    out,
                    "        inet {}  netmask {}",
                    address.address,
                    prefix_mask_v4(address.prefix)
                )?;
                if flags & libc::IFF_BROADCAST as u32 != 0 {
                    write!(
                        out,
                        "  broadcast {}",
                        address.broadcast.unwrap_or(Ipv4Addr::UNSPECIFIED)
                    )?;
                }
                if flags & libc::IFF_POINTOPOINT as u32 != 0 {
                    write!(
                        out,
                        "  destination {}",
                        address.peer.unwrap_or(address.address)
                    )?;
                }
                writeln!(out)?;
            } else {
                let (scope, name) = match address.scope {
                    0 => (0, "global"),
                    200 => (0x40, "site"),
                    253 => (0x20, "link"),
                    254 => (0x10, "host"),
                    _ => (0, "global"),
                };
                writeln!(
                    out,
                    "        inet6 {}  prefixlen {}  scopeid 0x{scope:x}<{name}>",
                    address.address, address.prefix
                )?;
            }
        }
        write_ifconfig_hardware(link, out)?;
        if let Some(stats) = link.stats {
            write_ifconfig_traffic(out, "RX", stats.rx_packets, stats.rx_bytes)?;
            writeln!(
                out,
                "        RX errors {}  dropped {}  overruns {}  frame {}",
                stats.rx_errors,
                stats.rx_dropped.wrapping_add(stats.rx_missed_errors),
                stats.rx_fifo_errors,
                stats.rx_frame_total
            )?;
            write_ifconfig_traffic(out, "TX", stats.tx_packets, stats.tx_bytes)?;
            writeln!(
                out,
                "        TX errors {}  dropped {} overruns {}  carrier {}  collisions {}",
                stats.tx_errors,
                stats.tx_dropped,
                stats.tx_fifo_errors,
                stats.tx_carrier_total,
                stats.collisions
            )?;
        }
        if let Some(map) = link.map
            && (map.irq != 0 || map.memory_start != 0 || map.dma != 0 || map.base_address >= 0x100)
        {
            write!(out, "        device ")?;
            if map.irq != 0 {
                write!(out, "interrupt {}  ", map.irq)?;
            }
            if map.base_address >= 0x100 {
                write!(out, "base 0x{:x}  ", map.base_address)?;
            }
            if map.memory_start != 0 {
                write!(
                    out,
                    "memory 0x{:x}-{:x}  ",
                    map.memory_start, map.memory_end
                )?;
            }
            if map.dma != 0 {
                write!(out, "dma 0x{:x}  ", map.dma)?;
            }
            writeln!(out)?;
        }
        writeln!(out)?;
    }
    Ok(())
}

fn order_ifconfig_ipv6(mut input: impl BufRead, addresses: &mut [AddressView]) -> io::Result<()> {
    // net-tools uses procfs order, which differs from the IPv6 netlink dump order.
    let mut positions = HashMap::new();
    let mut line = String::new();
    while input.read_line(&mut line)? != 0 {
        let mut fields = line.split_whitespace();
        if let (Some(address), Some(index)) = (fields.next(), fields.next())
            && let (Ok(address), Ok(index)) = (
                u128::from_str_radix(address, 16),
                u32::from_str_radix(index, 16),
            )
        {
            let position = positions.len() + 1;
            positions.insert((index, IpAddr::V6(Ipv6Addr::from(address))), position);
        }
        line.clear();
    }
    addresses.sort_by_key(|address| {
        if address.address.is_ipv4() {
            0
        } else {
            positions
                .get(&(address.index, address.address))
                .copied()
                .unwrap_or(usize::MAX)
        }
    });
    Ok(())
}

fn ifconfig_name_cmp(left: &str, right: &str) -> std::cmp::Ordering {
    let split = |name: &str| {
        name.as_bytes()
            .iter()
            .rposition(|byte| !byte.is_ascii_digit())
            .map_or(0, |index| index + 1)
    };
    let left_index = split(left);
    let right_index = split(right);
    left[..left_index]
        .cmp(&right[..right_index])
        .then_with(|| {
            let left_number = left[left_index..].parse::<u64>().unwrap_or(0);
            let right_number = right[right_index..].parse::<u64>().unwrap_or(0);
            left_number.cmp(&right_number)
        })
        .then_with(|| left.cmp(right))
}

fn write_ifconfig_hardware(link: &LinkView, out: &mut dyn Write) -> io::Result<()> {
    let (name, title) = match link.link_type {
        1 => ("ether", "Ethernet"),
        512 => ("ppp", "Point-to-Point Protocol"),
        768 => ("tunnel", "IPIP Tunnel"),
        772 => ("loop", "Local Loopback"),
        776 => ("sit", "IPv6-in-IPv4"),
        _ => ("unspec", "UNSPEC"),
    };
    write!(out, "        {name}")?;
    match link.link_type {
        1 => {
            if let Some(address) = &link.address
                && link.raw_address.iter().any(|byte| *byte != 0)
            {
                write!(out, " {address}")?;
            }
        }
        768 => write!(out, " ")?,
        512 | 772 | 776 => {}
        _ => {
            // net-tools uses the fixed sockaddr-sized UNSPEC representation.
            write!(out, " ")?;
            for index in 0..16 {
                if index != 0 {
                    write!(out, "-")?;
                }
                write!(
                    out,
                    "{:02X}",
                    link.raw_address.get(index).copied().unwrap_or(0)
                )?;
            }
        }
    }
    if let Some(length) = link.tx_queue_len {
        write!(out, "  txqueuelen {length}")?;
    }
    writeln!(out, "  ({title})")
}

fn write_ifconfig_traffic(
    out: &mut dyn Write,
    direction: &str,
    packets: u64,
    bytes: u64,
) -> io::Result<()> {
    let (divisor, unit) = [
        (1u64 << 60, "EiB"),
        (1u64 << 50, "PiB"),
        (1u64 << 40, "TiB"),
        (1u64 << 30, "GiB"),
        (1u64 << 20, "MiB"),
        (1u64 << 10, "KiB"),
    ]
    .into_iter()
    .find(|(divisor, _)| bytes > *divisor)
    .unwrap_or((1, "B"));
    let tenths = u128::from(bytes) * 10 / u128::from(divisor);
    writeln!(
        out,
        "        {direction} packets {packets}  bytes {bytes} ({}.{} {unit})",
        tenths / 10,
        tenths % 10
    )
}

pub fn ipcalc(args: Vec<OsString>) -> i32 {
    run_output("ipcalc", |out| run_ipcalc(args, out))
}

const IPCALC_USAGE: &str = concat!(
    "Usage: ipcalc [OPTION...]\n",
    "  -c, --check                     Validate IP address\n",
    "  -i, --info                      Print information (default)\n",
    "  -a, --address                   Display IP address\n",
    "  -b, --broadcast                 Display calculated IPv4 broadcast address\n",
    "  -m, --netmask                   Display netmask\n",
    "  -n, --network                   Display network address\n",
    "  -p, --prefix                    Display network prefix\n",
    "      --minaddr, --maxaddr        Display usable address range\n",
    "      --addresses, --addrspace    Display address count or address space\n",
    "  -4, --ipv4                      Require IPv4\n",
    "  -6, --ipv6                      Require IPv6\n",
    "  -j, --json                      JSON output\n",
    "      --no-decorate               Omit variable names\n",
    "  -v, --version                   Display AXE version\n",
    "  -?, --help                      Show this help message\n",
    "Numeric ADDRESS[/PREFIX] and an optional IPv4 NETMASK are supported.",
);

const CALC_ADDRESS: u16 = 1 << 0;
const CALC_NETMASK: u16 = 1 << 1;
const CALC_PREFIX: u16 = 1 << 2;
const CALC_BROADCAST: u16 = 1 << 3;
const CALC_NETWORK: u16 = 1 << 4;
const CALC_MIN: u16 = 1 << 5;
const CALC_MAX: u16 = 1 << 6;
const CALC_SPACE: u16 = 1 << 7;
const CALC_COUNT: u16 = 1 << 8;

fn ipcalc_error(message: impl std::fmt::Display) -> CommandError {
    CommandError::new(1, format!("ipcalc: {message}"))
}

fn run_ipcalc(args: Vec<OsString>, out: &mut dyn Write) -> Result<(), CommandError> {
    let values = utf8_args(args)?;
    let mut selected = 0;
    let mut info = false;
    let mut check = false;
    let mut version = false;
    let mut json = false;
    let mut no_decorate = false;
    let mut ipv4 = false;
    let mut ipv6 = false;
    let mut positional = [None; 3];
    let mut count = 0;
    let mut operands = false;
    for value in &values[1..] {
        let value = value.as_str();
        if !operands && value == "--" {
            operands = true;
            continue;
        }
        if !operands && value.starts_with("--") {
            match value {
                "--address" => selected |= CALC_ADDRESS,
                "--netmask" => selected |= CALC_NETMASK,
                "--prefix" => selected |= CALC_PREFIX,
                "--broadcast" => selected |= CALC_BROADCAST,
                "--network" => selected |= CALC_NETWORK,
                "--minaddr" => selected |= CALC_MIN,
                "--maxaddr" => selected |= CALC_MAX,
                "--addrspace" => selected |= CALC_SPACE,
                "--addresses" => selected |= CALC_COUNT,
                "--info" => info = true,
                "--check" => check = true,
                "--json" => json = true,
                "--no-decorate" => no_decorate = true,
                "--ipv4" => ipv4 = true,
                "--ipv6" => ipv6 = true,
                "--help" | "--usage" => return Err(CommandError::new(0, IPCALC_USAGE)),
                "--version" => version = true,
                _ => return Err(ipcalc_error(format_args!("unrecognized option '{value}'"))),
            }
        } else if !operands && value.starts_with('-') && value != "-" {
            for flag in value[1..].chars() {
                match flag {
                    'a' => selected |= CALC_ADDRESS,
                    'm' => selected |= CALC_NETMASK,
                    'p' => selected |= CALC_PREFIX,
                    'b' => selected |= CALC_BROADCAST,
                    'n' => selected |= CALC_NETWORK,
                    'i' => info = true,
                    'c' => check = true,
                    'j' => json = true,
                    '4' => ipv4 = true,
                    '6' => ipv6 = true,
                    '?' => return Err(CommandError::new(0, IPCALC_USAGE)),
                    'v' => version = true,
                    _ => return Err(ipcalc_error(format_args!("invalid option -- '{flag}'"))),
                }
            }
        } else {
            if let Some(slot) = positional.get_mut(count) {
                *slot = Some(value);
            }
            count += 1;
        }
    }
    if ipv4 && ipv6 {
        return Err(ipcalc_error("you cannot specify both IPv4 and IPv6"));
    }
    if (check && (info || selected != 0)) || (version && (check || info || selected != 0)) {
        return Err(ipcalc_error("you cannot mix these options"));
    }
    if version {
        writeln!(out, "ipcalc (axe) {}", env!("CARGO_PKG_VERSION"))?;
        return Ok(());
    }
    let address_spec = positional[0]
        .ok_or_else(|| ipcalc_error(format_args!("ip address expected\n{IPCALC_USAGE}")))?;
    let ipv6 = ipv6 || (!ipv4 && address_spec.contains(':'));
    let family = if ipv6 { "IPv6" } else { "IPv4" };
    if ipv6 && let Some(value) = positional[1] {
        return Err(ipcalc_error(format_args!(
            "unexpected argument: {value}\n{IPCALC_USAGE}"
        )));
    }
    let (address_text, inline_prefix) = if positional[1].is_none() {
        address_spec
            .split_once('/')
            .map_or((address_spec, None), |(address, prefix)| {
                (address, Some(prefix))
            })
    } else {
        (address_spec, None)
    };
    let width = if ipv6 { 128 } else { 32 };
    let prefix = if let Some(value) = inline_prefix.or(positional[1]) {
        parse_ipcalc_prefix(value, ipv6)
            .ok_or_else(|| ipcalc_error(format_args!("bad {family} prefix: {value}")))?
    } else {
        width
    };
    let address: IpAddr = address_text
        .parse()
        .ok()
        .filter(|address: &IpAddr| address.is_ipv6() == ipv6)
        .ok_or_else(|| ipcalc_error(format_args!("bad {family} address: {address_text}")))?;
    if let Some(value) = positional[2] {
        return Err(ipcalc_error(format_args!("unexpected argument: {value}")));
    }
    if check {
        return Ok(());
    }
    let (network, netmask, broadcast, range, hosts) = match address {
        IpAddr::V4(address) => {
            let mask = u32::from(prefix_mask_v4(prefix));
            let network = u32::from(address) & mask;
            let last = network | !mask;
            let broadcast = if prefix == 31 { u32::MAX } else { last };
            let (first, last) = if prefix < 31 {
                (network + 1, last - 1)
            } else {
                (network, last)
            };
            let range = (
                IpAddr::V4(Ipv4Addr::from(first)),
                IpAddr::V4(Ipv4Addr::from(last)),
            );
            let count = (1u128 << (32 - prefix)) - if prefix < 31 { 2 } else { 0 };
            (
                IpAddr::V4(Ipv4Addr::from(network)),
                IpAddr::V4(Ipv4Addr::from(mask)),
                Some(Ipv4Addr::from(broadcast)),
                range,
                IpCalcHosts(Some(count)),
            )
        }
        IpAddr::V6(address) => {
            let mask = if prefix == 0 {
                0
            } else {
                u128::MAX << (128 - prefix)
            };
            let network = u128::from(address) & mask;
            let range = (
                IpAddr::V6(Ipv6Addr::from(network)),
                IpAddr::V6(Ipv6Addr::from(network | !mask)),
            );
            (
                IpAddr::V6(Ipv6Addr::from(network)),
                IpAddr::V6(Ipv6Addr::from(mask)),
                None,
                range,
                IpCalcHosts(if prefix == 0 {
                    None
                } else {
                    Some(1u128 << (128 - prefix))
                }),
            )
        }
    };
    let space = ipcalc_address_space(network, prefix);
    let modern = info || selected == 0 || json;
    let mut output = IpCalcOutput {
        out,
        json,
        variables: !modern,
        no_decorate,
        first: true,
    };
    if json {
        output.out.write_all(b"{\n")?;
    }
    if modern {
        let single = prefix == width;
        if single || address != network {
            if let IpAddr::V6(address) = address {
                output.field("FULLADDRESS", "Full Address:\t", ExpandedIpv6(address))?;
            }
            output.field("ADDRESS", "Address:\t", address)?;
        }
        if !single || json {
            if let IpAddr::V6(network) = network {
                if json {
                    output.field("FULLNETWORK", "Full Network:\t", ExpandedIpv6(network))?;
                } else {
                    output.field(
                        "FULLNETWORK",
                        "Full Network:\t",
                        format_args!("{}/{prefix}", ExpandedIpv6(network)),
                    )?;
                }
            }
            if json {
                output.field("NETWORK", "Network:\t", network)?;
                output.field("NETMASK", "Netmask:\t", netmask)?;
                output.field("PREFIX", "Prefix:\t", prefix)?;
            } else {
                output.field("NETWORK", "Network:\t", format_args!("{network}/{prefix}"))?;
                output.field(
                    "NETMASK",
                    "Netmask:\t",
                    format_args!("{netmask} = {prefix}"),
                )?;
            }
            if let Some(broadcast) = broadcast {
                output.field("BROADCAST", "Broadcast:\t", broadcast)?;
            }
            if !json {
                writeln!(output.out)?;
            }
        }
        output.field("ADDRSPACE", "Address space:\t", space)?;
        if !single || json {
            output.field("MINADDR", "HostMin:\t", range.0)?;
            output.field("MAXADDR", "HostMax:\t", range.1)?;
            if ipv6 && prefix < 112 && !json {
                output.field(
                    "ADDRESSES",
                    "Hosts/Net:\t",
                    format_args!("2^({}) = {hosts}", 128 - prefix),
                )?;
            } else {
                output.field("ADDRESSES", "Hosts/Net:\t", hosts)?;
            }
        }
    } else {
        if selected & CALC_ADDRESS != 0 {
            output.field("ADDRESS", "", address)?;
        }
        if selected & CALC_NETMASK != 0 {
            output.field("NETMASK", "", netmask)?;
        }
        if selected & CALC_PREFIX != 0 {
            output.field("PREFIX", "", prefix)?;
        }
        if selected & CALC_BROADCAST != 0
            && let Some(broadcast) = broadcast
        {
            output.field("BROADCAST", "", broadcast)?;
        }
        if selected & CALC_NETWORK != 0 {
            output.field("NETWORK", "", network)?;
        }
        if selected & CALC_MIN != 0 {
            output.field("MINADDR", "", range.0)?;
        }
        if selected & CALC_MAX != 0 {
            output.field("MAXADDR", "", range.1)?;
        }
        if selected & CALC_SPACE != 0 {
            if space.contains(' ') {
                output.field("ADDRSPACE", "", format_args!("\"{space}\""))?;
            } else {
                output.field("ADDRSPACE", "", space)?;
            }
        }
        if selected & CALC_COUNT != 0 {
            output.field("ADDRESSES", "", hosts)?;
        }
    }
    if json {
        output.out.write_all(b"\n}\n")?;
    }
    Ok(())
}

fn parse_ipcalc_prefix(value: &str, ipv6: bool) -> Option<u8> {
    if let Ok(prefix) = value.parse::<u8>() {
        return (prefix <= if ipv6 { 128 } else { 32 }).then_some(prefix);
    }
    if ipv6 {
        return None;
    }
    let mask = u32::from(value.parse::<Ipv4Addr>().ok()?);
    let prefix = mask.leading_ones() as u8;
    (mask == u32::from(prefix_mask_v4(prefix))).then_some(prefix)
}

struct IpCalcOutput<'a> {
    out: &'a mut dyn Write,
    json: bool,
    variables: bool,
    no_decorate: bool,
    first: bool,
}

impl IpCalcOutput<'_> {
    fn field(
        &mut self,
        key: &str,
        label: &str,
        value: impl std::fmt::Display,
    ) -> Result<(), CommandError> {
        if self.json {
            if !self.first {
                self.out.write_all(b",\n")?;
            }
            write!(self.out, "  \"{key}\":")?;
            serde_json::to_writer(&mut *self.out, &value.to_string()).map_err(|error| {
                CommandError {
                    io_kind: error.io_error_kind(),
                    message: error.to_string(),
                    code: 1,
                }
            })?;
            self.first = false;
        } else {
            if self.variables {
                if !self.no_decorate {
                    write!(self.out, "{key}=")?;
                }
            } else {
                self.out.write_all(label.as_bytes())?;
            }
            writeln!(self.out, "{value}")?;
        }
        Ok(())
    }
}

struct ExpandedIpv6(Ipv6Addr);

impl std::fmt::Display for ExpandedIpv6 {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (index, segment) in self.0.segments().iter().enumerate() {
            if index != 0 {
                out.write_str(":")?;
            }
            write!(out, "{segment:04x}")?;
        }
        Ok(())
    }
}

struct IpCalcHosts(Option<u128>);

impl std::fmt::Display for IpCalcHosts {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(count) = self.0 {
            write!(out, "{count}")
        } else {
            out.write_str("340282366920938463463374607431768211456")
        }
    }
}

fn ipcalc_address_space(network: IpAddr, prefix: u8) -> &'static str {
    match network {
        IpAddr::V4(address) => match address.octets() {
            [0, ..] => "This host on this network",
            [10, ..] | [172, 16..=31, ..] | [192, 168, ..] => "Private Use",
            [100, 64..=127, ..] => "Shared Address Space",
            [127, ..] => "Loopback",
            [169, 254, ..] => "Link Local",
            [192, 0, 0, 0..=7] => "IPv4 Service Continuity Prefix",
            [192, 0, 0, 8] => "IPv4 dummy address",
            [192, 0, 0, 9] => "Port Control Protocol Anycast",
            [192, 0, 0, 10] => "Traversal Using Relays around NAT Anycast",
            [192, 0, 0, 170 | 171] => "NAT64/DNS64 Discovery",
            [192, 0, 0, _] => "IETF Protocol Assignments",
            [192, 0, 2, _] => "Documentation (TEST-NET-1)",
            [198, 51, 100, _] => "Documentation (TEST-NET-2)",
            [203, 0, 113, _] => "Documentation (TEST-NET-3)",
            [192, 88, 99, _] => "6 to 4 Relay Anycast (Deprecated)",
            [192, 31, 196, _] => "AS112-v4",
            [192, 52, 193, _] => "AMT",
            [192, 175, 48, _] => "Direct Delegation AS112 Service",
            [255, 255, 255, 255] => "Limited Broadcast",
            [198, 18..=19, ..] => "Benchmarking",
            [224..=239, ..] => "Multicast",
            [240..=255, ..] => "Reserved",
            _ => "Internet",
        },
        IpAddr::V6(address) => {
            let segments = address.segments();
            let raw = u128::from(address);
            let first = segments[0];
            let second = segments[1];
            if prefix == 128 && address.is_loopback() {
                "Loopback Address"
            } else if prefix == 128 && address.is_unspecified() {
                "Unspecified Address"
            } else if prefix >= 96 && raw >> 32 == 0xffff {
                "IPv4-mapped Address"
            } else if (prefix >= 96 && raw >> 32 == 0x0064ff9b0000000000000000)
                || (prefix >= 48 && raw >> 80 == 0x0064ff9b0001)
            {
                "IPv4-IPv6 Translat."
            } else if prefix >= 96 && raw >> 32 == 0x100000000000000000000000 {
                "Discard-Only Address Block"
            } else if prefix >= 32 && first == 0x2001 && second == 0 {
                "TEREDO"
            } else if prefix == 128 && raw == 0x20010001000000000000000000000001 {
                "Port Control Protocol Anycast"
            } else if prefix == 128 && raw == 0x20010001000000000000000000000002 {
                "Traversal Using Relays around NAT Anycast"
            } else if prefix >= 48 && raw >> 80 == 0x200100020000 {
                "Benchmarking"
            } else if prefix >= 32 && first == 0x2001 && second == 3 {
                "AMT"
            } else if prefix >= 48 && raw >> 80 == 0x200100040112 {
                "AS112-v6"
            } else if prefix >= 28 && first == 0x2001 && second & 0xfff0 == 0x10 {
                "Deprecated (previously ORCHID)"
            } else if prefix >= 28 && first == 0x2001 && second & 0xfff0 == 0x20 {
                "ORCHIDv2"
            } else if prefix >= 23 && first == 0x2001 && second & 0xff00 <= 0x100 {
                "IETF Protocol Assignments"
            } else if prefix >= 32 && first == 0x2001 && second == 0xdb8 {
                "Documentation"
            } else if first == 0x2002 {
                "6to4"
            } else if prefix >= 48 && raw >> 80 == 0x2620004f8000 {
                "Direct Delegation AS112 Service"
            } else if first & 0xe000 == 0x2000 {
                "Global Unicast"
            } else if first & 0xfe00 == 0xfc00 {
                "Unique Local Unicast"
            } else if first & 0xffc0 == 0xfe80 {
                "Link-Scoped Unicast"
            } else if first & 0xff00 == 0xff00 {
                "Multicast"
            } else {
                "Reserved"
            }
        }
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

#[derive(Default)]
struct LinkView {
    index: u32,
    flags: LinkFlags,
    link_type: u16,
    name: String,
    mtu: Option<u32>,
    qdisc: Option<String>,
    state: String,
    mode: String,
    group: u32,
    master: Option<u32>,
    peer: Option<u32>,
    netns: Option<i32>,
    address: Option<String>,
    raw_address: Vec<u8>,
    broadcast: Option<String>,
    permanent_address: Option<String>,
    alternate_names: Vec<String>,
    kind: Option<String>,
    tx_queue_len: Option<u32>,
    stats: Option<LinkStats>,
    map: Option<netlink_packet_route::link::Map>,
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
    rx_over_errors: u64,
    rx_missed_errors: u64,
    rx_fifo_errors: u64,
    rx_frame_total: u64,
    multicast: u64,
    tx_carrier_errors: u64,
    tx_carrier_total: u64,
    tx_fifo_errors: u64,
    collisions: u64,
}

impl LinkView {
    fn new(message: LinkMessage) -> Self {
        let mut link = Self {
            index: message.header.index,
            flags: message.header.flags,
            link_type: message.header.link_layer_type.into(),
            state: "UNKNOWN".into(),
            mode: "DEFAULT".into(),
            ..Self::default()
        };
        for attribute in message.attributes {
            match attribute {
                LinkAttribute::IfName(value) => link.name = value,
                LinkAttribute::Mtu(value) => link.mtu = Some(value),
                LinkAttribute::Qdisc(value) => link.qdisc = Some(value),
                LinkAttribute::OperState(value) => {
                    link.state =
                        name_of(u32::from(u8::from(value)), OPER_STATES, false).to_ascii_uppercase()
                }
                LinkAttribute::Mode(value) => {
                    link.mode = name_of(u32::from(u8::from(value)), LINK_MODES, false).into_owned()
                }
                LinkAttribute::Group(value) => link.group = value,
                LinkAttribute::Controller(value) => link.master = Some(value),
                LinkAttribute::Link(value) => link.peer = Some(value),
                LinkAttribute::LinkNetNsId(value) => link.netns = Some(value),
                LinkAttribute::Address(value) => {
                    link.address = Some(link_address(&value, message.header.link_layer_type));
                    link.raw_address = value;
                }
                LinkAttribute::Broadcast(value) => {
                    link.broadcast = Some(link_address(&value, message.header.link_layer_type))
                }
                LinkAttribute::PermAddress(value) => {
                    link.permanent_address =
                        Some(link_address(&value, message.header.link_layer_type))
                }
                LinkAttribute::PropList(values) => link.alternate_names.extend(
                    values.into_iter().filter_map(|value| match value {
                        Prop::AltIfName(name) => Some(name),
                        _ => None,
                    }),
                ),
                LinkAttribute::LinkInfo(values) => {
                    link.kind = values.into_iter().find_map(|value| match value {
                        LinkInfoAttribute::Kind(kind) => Some(kind.to_string()),
                        _ => None,
                    });
                }
                LinkAttribute::TxQueueLen(value) => link.tx_queue_len = Some(value),
                LinkAttribute::Map(value) => link.map = Some(value),
                LinkAttribute::Stats(value) if link.stats.is_none() => {
                    link.stats = Some(LinkStats {
                        rx_packets: value.rx_packets.into(),
                        tx_packets: value.tx_packets.into(),
                        rx_bytes: value.rx_bytes.into(),
                        tx_bytes: value.tx_bytes.into(),
                        rx_errors: value.rx_errors.into(),
                        tx_errors: value.tx_errors.into(),
                        rx_dropped: value.rx_dropped.into(),
                        tx_dropped: value.tx_dropped.into(),
                        rx_over_errors: value.rx_over_errors.into(),
                        rx_missed_errors: value.rx_missed_errors.into(),
                        rx_fifo_errors: value.rx_fifo_errors.into(),
                        rx_frame_total: u64::from(value.rx_length_errors)
                            + u64::from(value.rx_over_errors)
                            + u64::from(value.rx_crc_errors)
                            + u64::from(value.rx_frame_errors),
                        multicast: value.multicast.into(),
                        tx_carrier_errors: value.tx_carrier_errors.into(),
                        tx_carrier_total: u64::from(value.tx_carrier_errors)
                            + u64::from(value.tx_aborted_errors)
                            + u64::from(value.tx_window_errors)
                            + u64::from(value.tx_heartbeat_errors),
                        tx_fifo_errors: value.tx_fifo_errors.into(),
                        collisions: value.collisions.into(),
                    });
                }
                LinkAttribute::Stats64(value) => {
                    link.stats = Some(LinkStats {
                        rx_packets: value.rx_packets,
                        tx_packets: value.tx_packets,
                        rx_bytes: value.rx_bytes,
                        tx_bytes: value.tx_bytes,
                        rx_errors: value.rx_errors,
                        tx_errors: value.tx_errors,
                        rx_dropped: value.rx_dropped,
                        tx_dropped: value.tx_dropped,
                        rx_over_errors: value.rx_over_errors,
                        rx_missed_errors: value.rx_missed_errors,
                        rx_fifo_errors: value.rx_fifo_errors,
                        rx_frame_total: value
                            .rx_length_errors
                            .wrapping_add(value.rx_over_errors)
                            .wrapping_add(value.rx_crc_errors)
                            .wrapping_add(value.rx_frame_errors),
                        multicast: value.multicast,
                        tx_carrier_errors: value.tx_carrier_errors,
                        tx_carrier_total: value
                            .tx_carrier_errors
                            .wrapping_add(value.tx_aborted_errors)
                            .wrapping_add(value.tx_window_errors)
                            .wrapping_add(value.tx_heartbeat_errors),
                        tx_fifo_errors: value.tx_fifo_errors,
                        collisions: value.collisions,
                    });
                }
                _ => {}
            }
        }
        link
    }

    fn display_name<'a>(&'a self, snapshot: &Snapshot) -> Cow<'a, str> {
        match self.peer {
            None => Cow::Borrowed(&self.name),
            Some(0) => Cow::Owned(format!("{}@NONE", self.name)),
            Some(index) if self.netns.is_none() && snapshot.link(index).is_some() => {
                Cow::Owned(format!("{}@{}", self.name, snapshot.name(index)))
            }
            Some(index) => Cow::Owned(format!("{}@if{index}", self.name)),
        }
    }

    fn ip_flags(&self, snapshot: &Snapshot) -> Vec<&'static str> {
        let flags = self.flags.bits();
        let mut names = Vec::new();
        if flags & libc::IFF_UP as u32 != 0 && flags & libc::IFF_RUNNING as u32 == 0 {
            names.push("NO-CARRIER");
        }
        names.extend(
            LINK_FLAGS
                .iter()
                .filter_map(|(mask, name)| (flags & mask != 0).then_some(*name)),
        );
        if self.netns.is_none()
            && self.peer.is_some_and(|index| {
                index != 0
                    && snapshot
                        .link(index)
                        .is_none_or(|peer| !peer.flags.contains(LinkFlags::Up))
            })
        {
            names.push("M-DOWN");
        }
        names
    }

    fn json(
        &self,
        snapshot: &Snapshot,
        options: &IpOptions,
        object: Object,
    ) -> serde_json::Map<String, serde_json::Value> {
        let mut value = serde_json::Map::new();
        value.insert("ifname".into(), self.name.as_str().into());
        value.insert("operstate".into(), self.state.as_str().into());
        if let Some(peer) = self.peer {
            if peer == 0 {
                value.insert("link".into(), serde_json::Value::Null);
            } else if self.netns.is_some() {
                value.insert("link_index".into(), peer.into());
            } else {
                value.insert("link".into(), snapshot.name(peer).as_ref().into());
            }
        }
        if !options.brief {
            value.insert("ifindex".into(), self.index.into());
            insert_optional(&mut value, "link_netnsid", self.netns);
            value.insert("flags".into(), self.ip_flags(snapshot).into());
            insert_optional(&mut value, "mtu", self.mtu);
            insert_optional(&mut value, "qdisc", self.qdisc.as_deref());
            if let Some(master) = self.master {
                value.insert("master".into(), snapshot.name(master).as_ref().into());
            }
            if object == Object::Link {
                value.insert("linkmode".into(), self.mode.as_str().into());
            }
            value.insert(
                "group".into(),
                name_of(self.group, GROUPS, options.numeric).as_ref().into(),
            );
            insert_optional(&mut value, "txqlen", self.tx_queue_len);
            if options.family.is_none() || options.family == Some(AddressFamily::Packet) {
                value.insert(
                    "link_type".into(),
                    link_type_name(self.link_type, options.numeric)
                        .as_ref()
                        .into(),
                );
                insert_optional(&mut value, "address", self.address.as_deref());
                insert_optional(&mut value, "broadcast", self.broadcast.as_deref());
                if self.flags.contains(LinkFlags::Pointopoint) && self.broadcast.is_some() {
                    value.insert("link_pointtopoint".into(), true.into());
                }
                if self.permanent_address != self.address {
                    insert_optional(&mut value, "permaddr", self.permanent_address.as_deref());
                }
            }
            if !self.alternate_names.is_empty() {
                value.insert("altnames".into(), self.alternate_names.clone().into());
            }
        } else if object == Object::Link {
            insert_optional(&mut value, "address", self.address.as_deref());
            value.insert("flags".into(), self.ip_flags(snapshot).into());
        }
        if options.stats
            && let Some(stats) = self.stats
        {
            value.insert("stats64".into(), serde_json::json!({
                "rx": { "bytes": stats.rx_bytes, "packets": stats.rx_packets, "errors": stats.rx_errors, "dropped": stats.rx_dropped, "over_errors": stats.rx_over_errors, "multicast": stats.multicast },
                "tx": { "bytes": stats.tx_bytes, "packets": stats.tx_packets, "errors": stats.tx_errors, "dropped": stats.tx_dropped, "carrier_errors": stats.tx_carrier_errors, "collisions": stats.collisions }
            }));
        }
        value
    }

    fn write_stats(&self, out: &mut dyn Write) -> io::Result<()> {
        if let Some(stats) = self.stats {
            writeln!(
                out,
                "    RX: bytes  packets  errors  dropped  missed  mcast"
            )?;
            writeln!(
                out,
                "    {:<10} {:<8} {:<7} {:<8} {:<7} {}",
                stats.rx_bytes,
                stats.rx_packets,
                stats.rx_errors,
                stats.rx_dropped,
                stats.rx_missed_errors,
                stats.multicast
            )?;
            writeln!(
                out,
                "    TX: bytes  packets  errors  dropped  carrier collsns"
            )?;
            writeln!(
                out,
                "    {:<10} {:<8} {:<7} {:<8} {:<7} {}",
                stats.tx_bytes,
                stats.tx_packets,
                stats.tx_errors,
                stats.tx_dropped,
                stats.tx_carrier_errors,
                stats.collisions
            )?;
        }
        Ok(())
    }
}

struct AddressView {
    index: u32,
    address: IpAddr,
    peer: Option<IpAddr>,
    prefix: u8,
    scope: u8,
    flags: AddressFlags,
    protocol: Option<u8>,
    metric: Option<u32>,
    broadcast: Option<Ipv4Addr>,
    label: Option<std::ffi::CString>,
    valid_lft: Option<u32>,
    preferred_lft: Option<u32>,
}

impl AddressView {
    fn new(message: AddressMessage) -> Option<Self> {
        let mut local = None;
        let mut address = None;
        let mut view = Self {
            index: message.header.index,
            address: Ipv4Addr::UNSPECIFIED.into(),
            peer: None,
            prefix: message.header.prefix_len,
            scope: message.header.scope.into(),
            flags: AddressFlags::from_bits_retain(u32::from(message.header.flags.bits())),
            protocol: None,
            metric: None,
            broadcast: None,
            label: None,
            valid_lft: None,
            preferred_lft: None,
        };
        for attribute in message.attributes {
            match attribute {
                AddressAttribute::Local(value) => local = Some(value),
                AddressAttribute::Address(value) => address = Some(value),
                AddressAttribute::Broadcast(value) => view.broadcast = Some(value),
                AddressAttribute::Label(value) => view.label = std::ffi::CString::new(value).ok(),
                AddressAttribute::Flags(value) => view.flags = value,
                AddressAttribute::Protocol(value) => view.protocol = Some(value.into()),
                AddressAttribute::RoutePriority(value) => view.metric = Some(value),
                AddressAttribute::CacheInfo(value) => {
                    view.valid_lft = Some(value.ifa_valid);
                    view.preferred_lft = Some(value.ifa_preferred);
                }
                _ => {}
            }
        }
        view.address = local.or(address)?;
        view.peer = address.filter(|address| *address != view.address);
        Some(view)
    }

    fn matches(&self, options: &IpOptions) -> bool {
        family_matches(ip_family(self.address), options.family)
            && options.scope.is_none_or(|scope| self.scope == scope)
            && options
                .address_prefix
                .is_none_or(|prefix| prefix_matches(self.address, prefix.0, prefix.1))
            && options
                .address_protocol
                .is_none_or(|protocol| self.protocol.unwrap_or(0) == protocol)
            && options
                .address_flags
                .iter()
                .all(|(flag, set)| self.flags.contains(*flag) == *set)
            && options.label.as_deref().is_none_or(|label| {
                self.label
                    .as_deref()
                    .is_some_and(|actual| glob_label(label, actual))
            })
    }

    fn flag_names(&self) -> impl Iterator<Item = &'static str> + '_ {
        ADDRESS_FLAGS.iter().filter_map(move |(flag, name)| {
            if *flag == AddressFlags::Permanent {
                (!self.flags.contains(*flag)).then_some("dynamic")
            } else if self.flags.contains(*flag) {
                Some(
                    if *flag == AddressFlags::Secondary && self.address.is_ipv6() {
                        "temporary"
                    } else {
                        *name
                    },
                )
            } else {
                None
            }
        })
    }

    fn json(&self, options: &IpOptions) -> serde_json::Map<String, serde_json::Value> {
        let mut value = serde_json::Map::new();
        if !options.brief {
            value.insert("family".into(), family_name(self.address).into());
        }
        value.insert("local".into(), self.address.to_string().into());
        value.insert("prefixlen".into(), self.prefix.into());
        if let Some(peer) = self.peer {
            value.insert("address".into(), peer.to_string().into());
        }
        insert_optional(&mut value, "metric", self.metric);
        if options.brief {
            return value;
        }
        value.insert(
            "scope".into(),
            name_of(u32::from(self.scope), SCOPES, options.numeric)
                .as_ref()
                .into(),
        );
        insert_optional(
            &mut value,
            "broadcast",
            self.broadcast.map(|address| address.to_string()),
        );
        insert_optional(
            &mut value,
            "label",
            self.label.as_deref().and_then(|label| label.to_str().ok()),
        );
        for flag in self.flag_names() {
            value.insert(flag.into(), true.into());
        }
        if let Some(protocol) = self.protocol {
            value.insert(
                "protocol".into(),
                address_protocol_name(protocol, options.numeric)
                    .as_ref()
                    .into(),
            );
        }
        insert_optional(&mut value, "valid_life_time", self.valid_lft);
        insert_optional(&mut value, "preferred_life_time", self.preferred_lft);
        value
    }

    fn write_brief(&self, out: &mut dyn Write) -> io::Result<()> {
        write!(out, "{}", self.address)?;
        if let Some(peer) = self.peer {
            write!(out, " peer {peer}")?;
        }
        write!(out, "/{} ", self.prefix)?;
        if let Some(metric) = self.metric {
            write!(out, "metric {metric} ")?;
        }
        Ok(())
    }

    fn write_text(&self, out: &mut dyn Write, options: &IpOptions) -> io::Result<()> {
        write!(out, "{} {}", family_name(self.address), self.address)?;
        if let Some(peer) = self.peer {
            write!(out, " peer {peer}/{}", self.prefix)?;
        } else {
            write!(out, "/{}", self.prefix)?;
        }
        if let Some(metric) = self.metric {
            write!(out, " metric {metric}")?;
        }
        if let Some(broadcast) = self.broadcast {
            write!(out, " brd {broadcast}")?;
        }
        write!(
            out,
            " scope {}",
            name_of(u32::from(self.scope), SCOPES, options.numeric)
        )?;
        for flag in self.flag_names() {
            write!(out, " {flag}")?;
        }
        if let Some(protocol) = self.protocol.filter(|protocol| *protocol != 0) {
            write!(
                out,
                " proto {}",
                address_protocol_name(protocol, options.numeric)
            )?;
        }
        if let Some(label) = &self.label {
            write!(out, " {}", label.to_string_lossy())?;
        } else {
            out.write_all(b" ")?;
        }
        if let (Some(valid), Some(preferred)) = (self.valid_lft, self.preferred_lft) {
            write!(
                out,
                "{}       valid_lft {} preferred_lft {}",
                if options.oneline { "\\" } else { "\n" },
                format_lifetime(valid),
                format_lifetime(preferred)
            )?;
        }
        writeln!(out)
    }
}

fn glob_label(pattern: &CStr, value: &CStr) -> bool {
    // SAFETY: both C strings remain alive and fnmatch only reads them.
    unsafe { libc::fnmatch(pattern.as_ptr(), value.as_ptr(), 0) == 0 }
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

const TABLES: &[(u32, &str)] = &[
    (0, "unspec"),
    (253, "default"),
    (254, "main"),
    (255, "local"),
];
const GROUPS: &[(u32, &str)] = &[(0, "default")];
const SCOPES: &[(u32, &str)] = &[
    (0, "global"),
    (200, "site"),
    (253, "link"),
    (254, "host"),
    (255, "nowhere"),
];
const ADDRESS_PROTOCOLS: &[(u32, &str)] = &[
    (0, "unspec"),
    (1, "kernel_lo"),
    (2, "kernel_ra"),
    (3, "kernel_ll"),
];
const ROUTE_PROTOCOLS: &[(u32, &str)] = &[
    (0, "unspec"),
    (1, "redirect"),
    (2, "kernel"),
    (3, "boot"),
    (4, "static"),
    (8, "gated"),
    (9, "ra"),
    (10, "mrt"),
    (11, "zebra"),
    (12, "bird"),
    (13, "dnrouted"),
    (14, "xorp"),
    (15, "ntk"),
    (16, "dhcp"),
    (18, "keepalived"),
    (42, "babel"),
    (84, "ovn"),
    (99, "openr"),
    (186, "bgp"),
    (187, "isis"),
    (188, "ospf"),
    (189, "rip"),
    (192, "eigrp"),
];
const ROUTE_TYPES: &[(u32, &str)] = &[
    (0, "none"),
    (1, "unicast"),
    (2, "local"),
    (3, "broadcast"),
    (4, "anycast"),
    (5, "multicast"),
    (6, "blackhole"),
    (7, "unreachable"),
    (8, "prohibit"),
    (9, "throw"),
    (10, "nat"),
    (11, "xresolve"),
];
const NEIGHBOUR_STATES: &[(u32, &str)] = &[
    (0, "none"),
    (1, "incomplete"),
    (2, "reachable"),
    (4, "stale"),
    (8, "delay"),
    (16, "probe"),
    (32, "failed"),
    (64, "noarp"),
    (128, "permanent"),
];
const PREFERENCES: &[(u32, &str)] = &[(0, "medium"), (1, "high"), (3, "low")];
const OPER_STATES: &[(u32, &str)] = &[
    (0, "UNKNOWN"),
    (1, "NOTPRESENT"),
    (2, "DOWN"),
    (3, "LOWERLAYERDOWN"),
    (4, "TESTING"),
    (5, "DORMANT"),
    (6, "UP"),
];
const LINK_MODES: &[(u32, &str)] = &[(0, "DEFAULT"), (1, "DORMANT")];
const LINK_TYPES: &[(u32, &str)] = &[
    (0, "netrom"),
    (1, "ether"),
    (2, "eether"),
    (3, "ax25"),
    (4, "pronet"),
    (5, "chaos"),
    (6, "ieee802"),
    (7, "arcnet"),
    (8, "atalk"),
    (15, "dlci"),
    (19, "atm"),
    (23, "metricom"),
    (24, "ieee1394"),
    (32, "infiniband"),
    (256, "slip"),
    (257, "cslip"),
    (258, "slip6"),
    (259, "cslip6"),
    (260, "rsrvd"),
    (264, "adapt"),
    (270, "rose"),
    (271, "x25"),
    (272, "hwx25"),
    (280, "can"),
    (512, "ppp"),
    (513, "hdlc"),
    (516, "lapb"),
    (517, "ddcmp"),
    (518, "rawhdlc"),
    (768, "ipip"),
    (769, "tunnel6"),
    (770, "frad"),
    (771, "skip"),
    (772, "loopback"),
    (773, "ltalk"),
    (774, "fddi"),
    (775, "bif"),
    (776, "sit"),
    (777, "ip/ddp"),
    (778, "gre"),
    (779, "pimreg"),
    (780, "hippi"),
    (781, "ash"),
    (782, "econet"),
    (783, "irda"),
    (784, "fcpp"),
    (785, "fcal"),
    (786, "fcpl"),
    (787, "fcfb0"),
    (788, "fcfb1"),
    (789, "fcfb2"),
    (790, "fcfb3"),
    (791, "fcfb4"),
    (792, "fcfb5"),
    (793, "fcfb6"),
    (794, "fcfb7"),
    (795, "fcfb8"),
    (796, "fcfb9"),
    (797, "fcfb10"),
    (798, "fcfb11"),
    (799, "fcfb12"),
    (800, "tr"),
    (801, "ieee802.11"),
    (802, "ieee802.11/prism"),
    (803, "ieee802.11/radiotap"),
    (804, "ieee802.15.4"),
    (805, "ieee802.15.4/monitor"),
    (820, "phonet"),
    (821, "phonet_pipe"),
    (822, "caif"),
    (823, "gre6"),
    (824, "netlink"),
    (825, "6lowpan"),
    (65534, "none"),
    (65535, "void"),
];
const LINK_FLAGS: &[(u32, &str)] = &[
    (0x8, "LOOPBACK"),
    (0x2, "BROADCAST"),
    (0x10, "POINTOPOINT"),
    (0x1000, "MULTICAST"),
    (0x80, "NOARP"),
    (0x200, "ALLMULTI"),
    (0x100, "PROMISC"),
    (0x400, "MASTER"),
    (0x800, "SLAVE"),
    (0x4, "DEBUG"),
    (0x8000, "DYNAMIC"),
    (0x4000, "AUTOMEDIA"),
    (0x2000, "PORTSEL"),
    (0x20, "NOTRAILERS"),
    (0x1, "UP"),
    (0x10000, "LOWER_UP"),
    (0x20000, "DORMANT"),
    (0x40000, "ECHO"),
];
const ROUTE_FLAGS: &[(u32, &str)] = &[
    (1, "dead"),
    (4, "onlink"),
    (2, "pervasive"),
    (8, "offload"),
    (64, "trap"),
    (0x100, "notify"),
    (16, "linkdown"),
    (32, "unresolved"),
    (0x4000, "rt_offload"),
    (0x8000, "rt_trap"),
    (0x20000000, "rt_offload_failed"),
];
const CACHE_FLAGS: &[(u32, &str)] = &[
    (0x80000000, "local"),
    (0x40000000, "reject"),
    (0x20000000, "mc"),
    (0x10000000, "brd"),
    (0x08000000, "dst-nat"),
    (0x00800000, "src-nat"),
    (0x00400000, "masq"),
    (0x00020000, "dst-direct"),
    (0x04000000, "src-direct"),
    (0x00040000, "redirected"),
    (0x01000000, "redirect"),
    (0x00200000, "fastroute"),
    (0x00010000, "notify"),
    (0x00080000, "proxy"),
];
const ADDRESS_FLAGS: &[(AddressFlags, &str)] = &[
    (AddressFlags::Secondary, "secondary"),
    (AddressFlags::Nodad, "nodad"),
    (AddressFlags::Optimistic, "optimistic"),
    (AddressFlags::Dadfailed, "dadfailed"),
    (AddressFlags::Homeaddress, "home"),
    (AddressFlags::Deprecated, "deprecated"),
    (AddressFlags::Tentative, "tentative"),
    (AddressFlags::Permanent, "dynamic"),
    (AddressFlags::Managetempaddr, "mngtmpaddr"),
    (AddressFlags::Noprefixroute, "noprefixroute"),
    (AddressFlags::Mcautojoin, "autojoin"),
    (AddressFlags::StablePrivacy, "stable-privacy"),
];

fn address_flag(name: &str) -> Option<AddressFlags> {
    if matches!(name, "temporary" | "primary") {
        return Some(AddressFlags::Secondary);
    }
    if name == "permanent" {
        return Some(AddressFlags::Permanent);
    }
    ADDRESS_FLAGS
        .iter()
        .find_map(|(flag, candidate)| (*candidate == name).then_some(*flag))
}

fn parse_name(value: &str, names: &[(u32, &str)]) -> Option<u32> {
    names
        .iter()
        .find_map(|(number, name)| (*name == value).then_some(*number))
        .or_else(|| parse_number(value))
}

fn name_of<'a>(number: u32, names: &'a [(u32, &str)], numeric: bool) -> Cow<'a, str> {
    if !numeric && let Some((_, name)) = names.iter().find(|(value, _)| *value == number) {
        return Cow::Borrowed(name);
    }
    Cow::Owned(number.to_string())
}

fn address_protocol_name(protocol: u8, numeric: bool) -> Cow<'static, str> {
    if !numeric
        && let Some((_, name)) = ADDRESS_PROTOCOLS
            .iter()
            .find(|(value, _)| *value == u32::from(protocol))
    {
        return Cow::Borrowed(name);
    }
    Cow::Owned(format!("{protocol:#x}"))
}

const REALMS: &[(u32, &str)] = &[(0, "unknown")];
const DS_FIELDS: &[(u32, &str)] = &[
    (0, "default"),
    (0x28, "AF11"),
    (0x30, "AF12"),
    (0x38, "AF13"),
    (0x48, "AF21"),
    (0x50, "AF22"),
    (0x58, "AF23"),
    (0x68, "AF31"),
    (0x70, "AF32"),
    (0x78, "AF33"),
    (0x88, "AF41"),
    (0x90, "AF42"),
    (0x98, "AF43"),
    (0x20, "CS1"),
    (0x40, "CS2"),
    (0x60, "CS3"),
    (0x80, "CS4"),
    (0xa0, "CS5"),
    (0xc0, "CS6"),
    (0xe0, "CS7"),
    (0xb8, "EF"),
];

fn dsfield_name(tos: u8, numeric: bool) -> Cow<'static, str> {
    if !numeric
        && let Some((_, name)) = DS_FIELDS.iter().find(|(value, _)| *value == u32::from(tos))
    {
        return Cow::Borrowed(name);
    }
    Cow::Owned(format!("0x{tos:02x}"))
}

fn link_type_name(number: u16, numeric: bool) -> Cow<'static, str> {
    if !numeric
        && let Some((_, name)) = LINK_TYPES
            .iter()
            .find(|(value, _)| *value == u32::from(number))
    {
        return Cow::Borrowed(name);
    }
    Cow::Owned(format!("[{number}]"))
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
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut value = String::with_capacity(bytes.len().saturating_mul(3).saturating_sub(1));
    for (index, byte) in bytes.iter().enumerate() {
        if index != 0 {
            value.push(':');
        }
        value.push(HEX[usize::from(byte >> 4)] as char);
        value.push(HEX[usize::from(byte & 0xf)] as char);
    }
    value
}

fn ifconfig_flags(flags: u32) -> String {
    [
        (libc::IFF_UP, "UP"),
        (libc::IFF_BROADCAST, "BROADCAST"),
        (libc::IFF_DEBUG, "DEBUG"),
        (libc::IFF_LOOPBACK, "LOOPBACK"),
        (libc::IFF_POINTOPOINT, "POINTOPOINT"),
        (libc::IFF_NOTRAILERS, "NOTRAILERS"),
        (libc::IFF_RUNNING, "RUNNING"),
        (libc::IFF_NOARP, "NOARP"),
        (libc::IFF_PROMISC, "PROMISC"),
        (libc::IFF_ALLMULTI, "ALLMULTI"),
        (libc::IFF_SLAVE, "SLAVE"),
        (libc::IFF_MASTER, "MASTER"),
        (libc::IFF_MULTICAST, "MULTICAST"),
        (libc::IFF_DYNAMIC, "DYNAMIC"),
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
            let json =
                LinkView::new(link).json(&Snapshot::default(), &IpOptions::default(), Object::Link);
            for field in ["address", "broadcast"] {
                assert_eq!(json[field], expected, "{link_type:?} {field}");
            }
        }
    }

    fn request(args: &[&str]) -> Result<IpRequest, CommandError> {
        let values = args
            .iter()
            .map(|arg| (*arg).to_string())
            .collect::<Vec<_>>();
        let request = parse_ip(&values, None)?;
        assert!(request.output.is_none(), "expected query for {args:?}");
        Ok(request)
    }

    fn link(index: u32, name: &str, flags: u32, attributes: Vec<LinkAttribute>) -> LinkMessage {
        let mut message = LinkMessage::default();
        message.header.index = index;
        message.header.flags = LinkFlags::from_bits_retain(flags);
        message.header.link_layer_type = LinkLayerType::Ether;
        message.attributes = vec![
            LinkAttribute::IfName(name.into()),
            LinkAttribute::Mtu(1500),
            LinkAttribute::Address(vec![2, 0, 0, 0, 0, index as u8]),
            LinkAttribute::Broadcast(vec![255; 6]),
            LinkAttribute::Group(0),
        ];
        message.attributes.extend(attributes);
        message
    }

    fn snapshot(links: Vec<LinkMessage>) -> Snapshot {
        let links = links.into_iter().map(LinkView::new).collect::<Vec<_>>();
        let indices = links
            .iter()
            .enumerate()
            .map(|(position, link)| (link.index, position))
            .collect();
        Snapshot {
            links,
            indices,
            ..Snapshot::default()
        }
    }

    fn route(
        prefix: &str,
        table: u32,
        protocol: u8,
        kind: u8,
        attributes: Vec<RouteAttribute>,
    ) -> RouteView {
        let prefix = parse_prefix(prefix, None).unwrap();
        let mut message = RouteMessage::default();
        message.header.address_family = ip_family(prefix.0);
        message.header.destination_prefix_length = prefix.1;
        message.header.table = u8::try_from(table).unwrap_or(0);
        message.header.protocol = protocol.into();
        message.header.kind = kind.into();
        message.attributes = vec![
            RouteAttribute::Destination(route_address(prefix.0)),
            RouteAttribute::Table(table),
        ];
        message.attributes.extend(attributes);
        RouteView::new(message)
    }

    fn render(snapshot: &Snapshot, args: &[&str]) -> Result<String, CommandError> {
        let mut request = request(args)?;
        resolve_selectors(snapshot, &mut request.options, request.object)?;
        assert!(request.query.is_none());
        let mut output = Vec::new();
        render_ip(snapshot, request.object, &request.options, &mut output)?;
        Ok(String::from_utf8(output).unwrap())
    }

    fn render_json(snapshot: &Snapshot, args: &[&str]) -> serde_json::Value {
        let mut args = args.to_vec();
        args.insert(0, "-j");
        serde_json::from_str(&render(snapshot, &args).unwrap()).unwrap()
    }

    fn error(args: &[&str]) -> CommandError {
        match request(args) {
            Err(error) => error,
            Ok(_) => panic!("accepted {args:?}"),
        }
    }

    #[test]
    fn verbs_and_unsupported_options_do_not_disguise_writes_as_reads() {
        for args in [
            vec!["l", "s", "dev", "d0", "up"],
            vec!["l", "set"],
            vec!["a", "add"],
            vec!["n", "flush"],
            vec!["r", "replace"],
            vec!["rule", "delete"],
        ] {
            assert_eq!(error(&args).code, 1, "{args:?}");
        }
        for args in [
            vec!["-d", "l"],
            vec!["-r", "a"],
            vec!["--netnsid", "a"],
            vec!["-f", "mpls", "r"],
            vec!["l", ""],
            vec!["r", "table", "foo"],
            vec!["r", "table"],
            vec!["a", "scope"],
            vec!["l", "type"],
            vec!["n", "nud"],
            vec!["n", "to"],
        ] {
            assert_eq!(error(&args).code, 255, "{args:?}");
        }
        assert_eq!(error(&["r", "match", "bad"]).code, 1);
        assert_eq!(
            error(&["foo"]).message,
            "Object \"foo\" is unknown, try \"ip help\"."
        );
        for option in ["-json", "-brief", "-oneline", "-st", "-statistics"] {
            assert!(request(&[option, "a", "show"]).is_ok(), "{option}");
        }
        assert_eq!(request(&["a", "s"]).unwrap().object, Object::Address);
        assert_eq!(request(&["r", "s"]).unwrap().object, Object::Route);
    }

    #[test]
    fn lookup_validates_family_and_builds_an_oif_host_request() {
        let snapshot = snapshot(vec![link(4, "d0", 0, vec![])]);
        let mut request = request(&[
            "r", "get", "0x0a.1", "mark", "0x6", "uid", "1000", "dev", "d0",
        ])
        .unwrap();
        resolve_selectors(&snapshot, &mut request.options, request.object).unwrap();
        let query = request.query.unwrap();
        assert_eq!(query.destination.to_string(), "10.1.0.0");
        let message = route_get_request(&request.options, &query);
        assert_eq!(message.header.destination_prefix_length, 32);
        assert!(message.header.flags.contains(RouteFlags::LookupTable));
        assert_eq!(
            message.attributes,
            vec![
                RouteAttribute::Destination(RouteAddress::Inet(Ipv4Addr::new(10, 1, 0, 0))),
                RouteAttribute::Mark(6),
                RouteAttribute::Uid(1000),
                RouteAttribute::Oif(4),
            ]
        );
        for args in [
            vec!["r", "get", "192.0.2.1", "from", "::1"],
            vec!["-6", "r", "get", "192.0.2.1"],
        ] {
            assert_eq!(error(&args).code, 1);
        }
        assert_eq!(
            error(&["r", "get", "192.0.2.1", "mark", "not-a-number"]).code,
            255
        );
        assert_eq!(error(&["r", "get", "192.0.2.1", "uid", "-1"]).code, 255);
        let missing = render(&snapshot, &["r", "show", "dev", "nosuch"]).unwrap_err();
        assert_eq!(missing.code, 1);
        assert_eq!(missing.message, "Cannot find device \"nosuch\"");
    }

    #[test]
    fn existing_down_device_is_an_empty_filter_not_a_missing_device() {
        let snapshot = snapshot(vec![link(4, "d0", 0, vec![])]);
        assert_eq!(
            render(&snapshot, &["l", "show", "dev", "d0", "up"]).unwrap(),
            ""
        );
        assert_eq!(
            render_json(&snapshot, &["l", "show", "up", "dev", "d0"]),
            serde_json::json!([])
        );
        let missing = render(&snapshot, &["l", "show", "dev", "nosuch"]).unwrap_err();
        assert_eq!(missing.message, "Device \"nosuch\" does not exist.");
    }

    #[test]
    fn route_prefix_modes_are_exact_covering_and_nested() {
        let mut snapshot = Snapshot::default();
        for prefix in [
            "default",
            "10.0.0.0/8",
            "10.1.0.0/16",
            "10.1.2.0/24",
            "10.1.2.3",
            "10.2.0.0/16",
        ] {
            snapshot.routes.push(route(prefix, 254, 3, 1, vec![]));
        }
        for (selector, expected) in [
            (vec!["10.1.0.0/16"], vec!["10.1.0.0/16"]),
            (
                vec!["match", "10.1.2.3"],
                vec![
                    "default",
                    "10.0.0.0/8",
                    "10.1.0.0/16",
                    "10.1.2.0/24",
                    "10.1.2.3",
                ],
            ),
            (
                vec!["root", "10.1.0.0/16"],
                vec!["10.1.0.0/16", "10.1.2.0/24", "10.1.2.3"],
            ),
            (
                vec!["root", "10.1.0.0/16", "match", "10.1.2.3"],
                vec!["10.1.0.0/16", "10.1.2.0/24", "10.1.2.3"],
            ),
            (vec!["10.1.2.3"], vec!["10.1.2.3"]),
            (vec!["default"], vec!["default"]),
        ] {
            let mut args = vec!["r", "show"];
            args.extend(selector);
            let json = render_json(&snapshot, &args);
            let destinations = json
                .as_array()
                .unwrap()
                .iter()
                .map(|route| route["dst"].as_str().unwrap())
                .collect::<Vec<_>>();
            assert_eq!(destinations, expected, "{args:?}");
        }
    }

    #[test]
    fn route_tables_numeric_protocols_and_types_share_text_and_json_selection() {
        let snapshot = Snapshot {
            routes: vec![
                route("10.0.0.0/8", 100, 99, 6, vec![]),
                route(
                    "2001:db8::/32",
                    100,
                    80,
                    9,
                    vec![RouteAttribute::Priority(0)],
                ),
                route("192.0.2.0/24", 254, 18, 1, vec![]),
                route("198.51.100.0/24", 254, 1, 1, vec![]),
            ],
            ..Snapshot::default()
        };
        let all = render(&snapshot, &["r", "show", "table", "all"]).unwrap();
        assert!(all.contains("blackhole 10.0.0.0/8 table 100 proto openr"));
        assert!(all.contains("throw 2001:db8::/32 table 100 proto 80 metric 0"));
        assert_eq!(
            render_json(&snapshot, &["r", "show", "table", "0"])
                .as_array()
                .unwrap()
                .len(),
            4
        );
        for protocol in ["18", "80", "1"] {
            let args = ["r", "show", "table", "all", "proto", protocol];
            let json = render_json(&snapshot, &args);
            assert_eq!(json.as_array().unwrap().len(), 1, "{protocol}");
            assert!(json[0].get("protocol").is_none());
            assert!(!render(&snapshot, &args).unwrap().contains(" proto "));
        }
        let blackhole = render_json(&snapshot, &["r", "show", "table", "100", "proto", "99"]);
        assert_eq!(
            blackhole[0],
            serde_json::json!({"type":"blackhole","dst":"10.0.0.0/8","flags":[]})
        );
        for (family, kind, destination) in [("-4", "6", "10.0.0.0/8"), ("-6", "9", "2001:db8::/32")]
        {
            let args = ["-N", family, "r", "show", "table", "100", "type", kind];
            assert!(
                render(&snapshot, &args)
                    .unwrap()
                    .starts_with(&format!("{kind} {destination} "))
            );
            let json = render_json(&snapshot, &args);
            assert_eq!(json.as_array().unwrap().len(), 1);
            assert_eq!(json[0]["type"], kind);
        }
        assert!(
            render(&snapshot, &["-N", "r", "show", "table", "all"])
                .unwrap()
                .contains("proto 99")
        );
    }

    #[test]
    fn multipath_metrics_and_route_attributes_are_not_lost_or_reordered() {
        use netlink_packet_route::route::{RouteNextHop, RouteNextHopFlags};
        let mut snapshot = snapshot(vec![link(4, "d0", 0, vec![]), link(5, "d1", 0, vec![])]);
        let mut first = RouteNextHop::default();
        first.interface_index = 4;
        first.attributes = vec![RouteAttribute::Gateway(RouteAddress::Inet(Ipv4Addr::new(
            10, 0, 0, 1,
        )))];
        let mut second = RouteNextHop::default();
        second.interface_index = 5;
        second.hops = 2;
        second.flags = RouteNextHopFlags::Linkdown;
        snapshot.routes.push(route(
            "10.5.0.0/16",
            254,
            4,
            1,
            vec![
                RouteAttribute::Priority(100),
                RouteAttribute::Metrics(vec![
                    RouteMetric::Advmss(1360),
                    RouteMetric::Rtt(80),
                    RouteMetric::Mtu(1400),
                    RouteMetric::Window(2000),
                ]),
                RouteAttribute::MultiPath(vec![first, second]),
            ],
        ));
        assert_eq!(
            render(&snapshot, &["r", "show", "dev", "d0"]).unwrap(),
            "10.5.0.0/16 proto static metric 100 mtu 1400 window 2000 rtt 10ms advmss 1360 \n\tnexthop via 10.0.0.1 dev d0 weight 1 \n\tnexthop dev d1 weight 3 linkdown \n"
        );
        let oneline = render(&snapshot, &["-o", "r", "show"]).unwrap();
        assert_eq!(oneline.lines().count(), 1);
        assert!(oneline.contains("\\\tnexthop"));
        let json = render_json(&snapshot, &["r", "show", "dev", "d0"]);
        assert_eq!(
            json[0]["nexthops"][0],
            serde_json::json!({"gateway":"10.0.0.1","dev":"d0","weight":1,"flags":[]})
        );
        assert_eq!(
            json[0]["nexthops"][1]["flags"],
            serde_json::json!(["linkdown"])
        );
        assert_eq!(
            json[0]["metrics"],
            serde_json::json!([{"mtu":1400,"window":2000,"rtt":10,"advmss":1360}])
        );
        assert!(json[0].get("gateway").is_none());
    }

    #[test]
    fn ipv6_preference_expiry_and_lookup_metadata_use_the_shared_route_view() {
        let mut snapshot = snapshot(vec![link(4, "d0", 0, vec![])]);
        let mut cache = RouteCacheInfo::parse(&[0; 32]).unwrap();
        cache.expires = user_hz() * 30;
        let mut entry = route(
            "2001:db8::/32",
            100,
            9,
            2,
            vec![
                RouteAttribute::Oif(4),
                RouteAttribute::Priority(0),
                RouteAttribute::Preference(1.into()),
                RouteAttribute::CacheInfo(cache),
                RouteAttribute::NhId(7),
                RouteAttribute::Uid(1000),
            ],
        );
        entry.tos = 16;
        entry.flags = RouteFlags::Linkdown.bits();
        snapshot.routes.push(entry);
        let text = render(&snapshot, &["-6", "r", "show", "table", "all"]).unwrap();
        assert_eq!(
            text,
            "local 2001:db8::/32 nhid 7 tos 0x10 dev d0 table 100 proto ra metric 0 linkdown uid 1000 expires 30sec pref high\n"
        );
        let json = render_json(&snapshot, &["-6", "r", "show", "table", "all"]);
        assert_eq!(json[0]["pref"], "high");
        assert_eq!(json[0]["metric"], 0);
        assert_eq!(json[0]["expires"], 30);
        let mut cached = route(
            "127.0.0.1",
            255,
            2,
            2,
            vec![RouteAttribute::Uid(1000), RouteAttribute::Oif(4)],
        );
        cached.flags = RouteFlags::Cloned.bits() | 0x80000000;
        let mut query = RouteGet {
            destination: "127.0.0.1".parse().unwrap(),
            source: None,
            mark: None,
            uid: None,
        };
        let mut output = Vec::new();
        render_routes(
            &snapshot,
            std::iter::once(&cached),
            &IpOptions::default(),
            Some(&query),
            &mut output,
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "local 127.0.0.1 dev d0 table local uid 1000 \n    cache <local> \n"
        );
        query.uid = Some(1000);
        let mut output = Vec::new();
        render_routes(
            &snapshot,
            std::iter::once(&cached),
            &IpOptions::default(),
            Some(&query),
            &mut output,
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "local 127.0.0.1 dev d0 table local uid 1000 \n    cache <local> \n"
        );
        assert_eq!(
            cached.json(&snapshot, &IpOptions::default(), Some(&query))["uid"],
            1000
        );
    }

    #[test]
    fn neighbours_apply_every_selector_and_keep_router_outside_state() {
        let mut snapshot = snapshot(vec![
            link(4, "d0", 0, vec![LinkAttribute::Controller(8)]),
            link(5, "d1", 0, vec![]),
            link(8, "blue", 0, vec![]),
        ]);
        snapshot.links[2].kind = Some("vrf".into());
        for (address, index, state, flags) in [
            ("192.0.2.1", 4, 2, NeighbourFlags::Router),
            ("192.0.2.2", 4, 0, NeighbourFlags::empty()),
            ("192.0.2.3", 4, 64, NeighbourFlags::empty()),
            ("192.0.2.4", 5, 2, NeighbourFlags::empty()),
            ("2001:db8::1", 4, 4, NeighbourFlags::empty()),
        ] {
            snapshot.neighbours.push(NeighbourView {
                destination: address.parse().unwrap(),
                index,
                lladdr: None,
                state,
                flags,
                probes: None,
            });
        }
        let args = [
            "n",
            "show",
            "to",
            "192.0.2.0/24",
            "vrf",
            "blue",
            "dev",
            "d0",
        ];
        assert_eq!(
            render(&snapshot, &args).unwrap(),
            "192.0.2.1 router REACHABLE \n"
        );
        assert_eq!(
            render_json(&snapshot, &args),
            serde_json::json!([{"dst":"192.0.2.1","router":true,"state":["REACHABLE"]}])
        );
        let all = render(&snapshot, &["n", "show", "dev", "d0", "nud", "all"]).unwrap();
        assert!(all.contains("192.0.2.2 \n"));
        assert!(!all.contains("NONE"));
        assert!(all.contains("NOARP"));
        let states = render_json(
            &snapshot,
            &["n", "show", "nud", "reachable", "nud", "stale"],
        );
        assert_eq!(states.as_array().unwrap().len(), 3);
    }

    fn address(
        index: u32,
        local: &str,
        scope: u8,
        flags: AddressFlags,
        attributes: Vec<AddressAttribute>,
    ) -> AddressView {
        let mut message = AddressMessage::default();
        message.header.index = index;
        message.header.family = ip_family(local.parse().unwrap());
        message.header.prefix_len = if message.header.family == AddressFamily::Inet {
            24
        } else {
            64
        };
        message.header.scope = scope.into();
        message.attributes = vec![
            AddressAttribute::Flags(flags),
            AddressAttribute::Local(local.parse().unwrap()),
        ];
        message.attributes.extend(attributes);
        AddressView::new(message).unwrap()
    }

    #[test]
    fn address_filters_and_flags_are_identical_in_text_brief_and_json() {
        use netlink_packet_route::address::CacheInfo;
        let mut snapshot = snapshot(vec![
            link(4, "d0", 1, vec![]),
            link(5, "empty", 1, vec![]),
            link(1, "lo", 1, vec![]),
        ]);
        let mut lifetime = CacheInfo::default();
        lifetime.ifa_valid = u32::MAX;
        lifetime.ifa_preferred = u32::MAX;
        snapshot.addresses = vec![
            address(
                4,
                "192.0.2.1",
                0,
                AddressFlags::Nodad | AddressFlags::Noprefixroute,
                vec![
                    AddressAttribute::Label("d0".into()),
                    AddressAttribute::CacheInfo(lifetime),
                ],
            ),
            address(1, "127.0.0.1", 254, AddressFlags::Permanent, vec![]),
            address(
                4,
                "2001:db8::1",
                0,
                AddressFlags::Permanent | AddressFlags::Nodad,
                vec![AddressAttribute::Protocol(3.into())],
            ),
        ];
        let text = render(
            &snapshot,
            &["-4", "a", "show", "scope", "0", "up", "dev", "d0"],
        )
        .unwrap();
        assert!(text.contains("scope global nodad dynamic noprefixroute d0"));
        assert!(!text.contains("link/"));
        assert!(!text.contains("127.0.0.1"));
        let json = render_json(&snapshot, &["a", "show", "scope", "global"]);
        assert_eq!(json[0]["addr_info"][0]["dynamic"], true);
        assert_eq!(json[0]["addr_info"][1]["protocol"], "kernel_ll");
        assert_eq!(
            render_json(&snapshot, &["a", "show", "proto", "kernel_ll"])[0]["addr_info"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let brief = render(&snapshot, &["-br", "-4", "a"]).unwrap();
        assert_eq!(brief.lines().count(), 2);
        assert!(!brief.contains("empty"));
        assert!(
            render_json(&snapshot, &["a", "show", "label", "d[0-9]"])[0]["addr_info"][0]["nodad"]
                .as_bool()
                .unwrap()
        );
    }

    #[test]
    fn address_scope_filters_keep_empty_links_but_not_filtered_nonempty_links() {
        let mut snapshot = snapshot(vec![
            link(1, "lo", 0, vec![]),
            link(4, "d0", 0, vec![]),
            link(5, "empty", 0, vec![]),
        ]);
        snapshot.addresses = vec![
            address(1, "127.0.0.1", 254, AddressFlags::Permanent, vec![]),
            address(4, "192.0.2.1", 0, AddressFlags::Permanent, vec![]),
            address(4, "2001:db8::1", 0, AddressFlags::Permanent, vec![]),
            address(4, "fe80::1", 253, AddressFlags::Permanent, vec![]),
        ];
        for scope in ["global", "0"] {
            let args = ["a", "show", "scope", scope];
            let text = render(&snapshot, &args).unwrap();
            let names = text
                .lines()
                .filter(|line| !line.starts_with(' '))
                .map(|line| line.split_whitespace().nth(1).unwrap())
                .collect::<Vec<_>>();
            assert_eq!(names, ["d0:", "empty:"], "{scope}");
            assert!(!text.contains("fe80::1"), "{scope}");
            let json = render_json(&snapshot, &args);
            let links = json.as_array().unwrap();
            assert_eq!(
                links
                    .iter()
                    .map(|link| link["ifname"].as_str().unwrap())
                    .collect::<Vec<_>>(),
                ["d0", "empty"],
                "{scope}"
            );
            assert_eq!(
                links[0]["addr_info"],
                serde_json::json!([
                    {"family":"inet","local":"192.0.2.1","prefixlen":24,"scope":"global"},
                    {"family":"inet6","local":"2001:db8::1","prefixlen":64,"scope":"global"}
                ])
            );
            assert_eq!(links[1]["addr_info"], serde_json::json!([]));
            let brief = render(&snapshot, &["-br", "a", "show", "scope", scope]).unwrap();
            assert_eq!(
                brief
                    .lines()
                    .map(|line| line.split_whitespace().next().unwrap())
                    .collect::<Vec<_>>(),
                ["d0", "empty"],
                "{scope}"
            );
        }
    }

    #[test]
    fn ipv6_address_headers_project_without_changing_ipv4_or_link_headers() {
        let flags = (libc::IFF_BROADCAST | libc::IFF_NOARP | libc::IFF_UP | libc::IFF_RUNNING)
            as u32
            | LinkFlags::LowerUp.bits();
        let mut snapshot = snapshot(vec![link(
            4,
            "d0",
            flags,
            vec![
                LinkAttribute::Qdisc("noqueue".into()),
                LinkAttribute::Group(5),
                LinkAttribute::TxQueueLen(1000),
            ],
        )]);
        snapshot.addresses = vec![
            address(4, "192.0.2.1", 0, AddressFlags::Permanent, vec![]),
            address(4, "2001:db8::1", 0, AddressFlags::Permanent, vec![]),
        ];
        for (args, expected) in [
            (
                vec!["-6", "a", "show"],
                "4: d0: <BROADCAST,NOARP,UP,LOWER_UP> mtu 1500 state UNKNOWN ",
            ),
            (
                vec!["-4", "a", "show"],
                "4: d0: <BROADCAST,NOARP,UP,LOWER_UP> mtu 1500 qdisc noqueue state UNKNOWN group 5 qlen 1000",
            ),
            (
                vec!["-6", "l", "show"],
                "4: d0: <BROADCAST,NOARP,UP,LOWER_UP> mtu 1500 qdisc noqueue state UNKNOWN mode DEFAULT group 5 qlen 1000",
            ),
        ] {
            let text = render(&snapshot, &args).unwrap();
            assert_eq!(text.lines().next().unwrap(), expected, "{args:?}");
            let json = render_json(&snapshot, &args);
            if args[1] == "a" && args[0] == "-6" {
                assert_eq!(json[0]["mtu"], 1500);
                assert_eq!(json[0]["operstate"], "UNKNOWN");
                assert_eq!(json[0]["qdisc"], "noqueue", "{args:?}");
                assert_eq!(json[0]["group"], "5", "{args:?}");
                assert_eq!(json[0]["txqlen"], 1000, "{args:?}");
                assert_eq!(json[0]["addr_info"][0]["local"], "2001:db8::1");
            } else {
                assert_eq!(json[0]["qdisc"], "noqueue", "{args:?}");
                assert_eq!(json[0]["group"], "5", "{args:?}");
                assert_eq!(json[0]["txqlen"], 1000, "{args:?}");
            }
        }
        for args in [
            vec!["-6", "a", "show"],
            vec!["-6", "a", "show", "dev", "d0"],
        ] {
            assert_eq!(
                render_json(&snapshot, &args),
                serde_json::json!([{
                    "ifindex":4, "ifname":"d0", "operstate":"UNKNOWN",
                    "flags":["BROADCAST","NOARP","UP","LOWER_UP"],
                    "mtu":1500, "qdisc":"noqueue", "group":"5", "txqlen":1000,
                    "addr_info":[{
                        "family":"inet6", "local":"2001:db8::1", "prefixlen":64, "scope":"global"
                    }]
                }]),
                "{args:?}"
            );
        }
    }

    #[test]
    fn link_views_preserve_master_peer_group_and_point_to_point_addresses() {
        let mut tunnel = link(
            4,
            "tun0",
            libc::IFF_UP as u32 | libc::IFF_POINTOPOINT as u32,
            vec![
                LinkAttribute::Link(5),
                LinkAttribute::Group(5),
                LinkAttribute::Controller(8),
            ],
        );
        tunnel.header.link_layer_type = LinkLayerType::Tunnel;
        tunnel
            .attributes
            .push(LinkAttribute::Address(vec![192, 0, 2, 1]));
        tunnel
            .attributes
            .push(LinkAttribute::Broadcast(vec![192, 0, 2, 2]));
        let mut snapshot = snapshot(vec![
            tunnel,
            link(5, "d0", 0, vec![]),
            link(8, "br0", 0, vec![]),
        ]);
        let text = render(&snapshot, &["l", "show", "dev", "tun0"]).unwrap();
        assert!(text.contains("tun0@d0: <NO-CARRIER,POINTOPOINT,UP,M-DOWN>"));
        assert!(text.contains("master br0"));
        assert!(text.contains("group 5"));
        assert!(text.contains("link/ipip 192.0.2.1 peer 192.0.2.2"));
        snapshot.addresses.push(address(
            4,
            "192.0.2.1",
            0,
            AddressFlags::Permanent,
            vec![AddressAttribute::Address("192.0.2.2".parse().unwrap())],
        ));
        assert!(
            render(&snapshot, &["a", "show", "master", "br0"])
                .unwrap()
                .contains("inet 192.0.2.1 peer 192.0.2.2/24")
        );
        let json = render_json(&snapshot, &["a", "show", "dev", "tun0"]);
        assert_eq!(json[0]["addr_info"][0]["local"], "192.0.2.1");
        assert_eq!(json[0]["addr_info"][0]["address"], "192.0.2.2");
        assert!(
            render(&snapshot, &["-br", "l", "show"])
                .unwrap()
                .contains("<NO-CARRIER,POINTOPOINT,UP,M-DOWN>")
        );
    }

    fn arp_output(args: &[&str], input: &str) -> String {
        let options = parse_arp(
            &args
                .iter()
                .map(|arg| (*arg).to_string())
                .collect::<Vec<_>>(),
        )
        .unwrap();
        let host = options
            .host
            .as_deref()
            .map(resolve_arp_host)
            .transpose()
            .unwrap();
        let mut output = Vec::new();
        render_arp(io::Cursor::new(input), &options, host, &mut output).unwrap();
        String::from_utf8(output).unwrap()
    }

    #[test]
    fn arp_uses_proc_atf_flags_without_ip_or_mac_heuristics() {
        let input = "IP address HW type Flags HW address Mask Device\n\
            10.0.0.5 0x1 0x6 ff:ff:ff:ff:ff:ff * d0\n\
            10.0.0.6 0x1 0x0 00:00:00:00:00:00 * d0\n";
        assert_eq!(
            arp_output(&["-an"], input),
            "? (10.0.0.5) at ff:ff:ff:ff:ff:ff [ether] PERM on d0\n? (10.0.0.6) at <incomplete> on d0\n"
        );
        let table = arp_output(&["-ne"], input);
        assert!(table.contains("ff:ff:ff:ff:ff:ff   CM"));
        assert!(table.contains("(incomplete)"));
        assert!(!table.contains("ether   (incomplete)"));
        assert_eq!(arp_output(&["-nea"], input), arp_output(&["-nae"], input));
        assert_eq!(
            arp_output(&["-ni", "d0"], input),
            arp_output(&["-nid0"], input)
        );
        assert_eq!(
            arp_output(&["-n", "--device=d0"], input),
            arp_output(&["-nid0"], input)
        );
    }

    #[test]
    fn arp_host_resolution_and_no_match_modes_have_net_tools_semantics() {
        let input = "IP address HW type Flags HW address Mask Device\n10.0.0.2 0x1 0x2 02:00:00:00:00:02 * d0\n";
        assert!(arp_output(&["-n", "10.2"], input).contains("10.0.0.2"));
        assert_eq!(
            arp_output(&["-n", "10.3"], input),
            "10.3 (10.0.0.3) -- no entry\n"
        );
        assert_eq!(
            arp_output(&["-an", "10.3"], input),
            "arp: in 1 entries no match found.\n"
        );
        assert_eq!(
            arp_output(&["-ni", "nosuch"], input),
            "arp: in 1 entries no match found.\n"
        );
        assert_eq!(
            arp_output(&["-n", "--device="], input),
            arp_output(&["-n"], input)
        );
        let missing = resolve_arp_host("bad\0host").unwrap_err();
        assert_eq!(missing.code, 255);
        assert!(missing.message.ends_with(": Host name lookup failure"));
        let invalid = match parse_arp(&["-nx".into()]) {
            Err(error) => error,
            Ok(_) => panic!("accepted invalid ARP option"),
        };
        assert_eq!(invalid.code, 3);
        assert_eq!(invalid.message, "arp: invalid option -- 'x'");
    }

    #[test]
    fn ifconfig_missing_device_keeps_short_header_and_unprefixed_error() {
        let snapshot = snapshot(vec![link(4, "d0", 0, vec![])]);
        let mut output = Vec::new();
        let error = ifconfig_device(&snapshot, Some("nosuch"), true, &mut output).unwrap_err();
        assert_eq!(
            error.message,
            "nosuch: error fetching interface information: Device not found"
        );
        assert!(String::from_utf8(output).unwrap().starts_with("Iface "));
        let mut output = Vec::new();
        let index = ifconfig_device(&snapshot, Some("d0"), false, &mut output).unwrap();
        render_ifconfig(&snapshot, false, false, index, &mut output).unwrap();
        assert!(
            String::from_utf8(output)
                .unwrap()
                .starts_with("d0: flags=0<>")
        );
    }

    #[test]
    fn renderer_preserves_output_errors_in_text_and_json() {
        struct FailedOutput(io::ErrorKind);
        impl Write for FailedOutput {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::from(self.0))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut snapshot = Snapshot::default();
        snapshot.routes.push(route("default", 254, 3, 1, vec![]));
        for json in [false, true] {
            let options = IpOptions {
                json,
                ..IpOptions::default()
            };
            for kind in [io::ErrorKind::BrokenPipe, io::ErrorKind::StorageFull] {
                let error = render_ip(&snapshot, Object::Route, &options, &mut FailedOutput(kind))
                    .unwrap_err();
                assert_eq!(error.io_kind, Some(kind));
            }
        }
    }

    #[test]
    fn route_realms_and_dsfields_use_static_names_and_numeric_output() {
        let mut snapshot = Snapshot::default();
        let mut entry = route(
            "192.0.2.0/24",
            254,
            3,
            1,
            vec![RouteAttribute::Realm(RouteRealm {
                source: 7,
                destination: 0,
            })],
        );
        entry.tos = 0x28;
        snapshot.routes.push(entry);
        assert_eq!(
            render(&snapshot, &["r", "show"]).unwrap(),
            "192.0.2.0/24 tos AF11 realms 7/unknown \n"
        );
        assert_eq!(
            render(&snapshot, &["-N", "r", "show"]).unwrap(),
            "192.0.2.0/24 tos 0x28 realms 7/0 \n"
        );
        assert_eq!(
            render_json(&snapshot, &["r", "show"])[0]["flow"],
            serde_json::json!({"from":"7","to":"unknown"})
        );
    }

    #[test]
    fn rule_table_filters_use_extended_table_ids_in_text_and_json() {
        let mut local = RuleMessage::default();
        local.header.table = 255;
        let mut main = RuleMessage::default();
        main.header.table = 254;
        main.attributes.push(RuleAttribute::Priority(32766));
        let mut extended = RuleMessage::default();
        extended.attributes = vec![RuleAttribute::Table(1000), RuleAttribute::Priority(100)];
        let snapshot = Snapshot {
            rules: vec![local, main, extended],
            ..Snapshot::default()
        };
        assert_eq!(
            render(&snapshot, &["rule", "show", "table", "local"]).unwrap(),
            "0:\tfrom all lookup local\n"
        );
        assert_eq!(
            render(&snapshot, &["rule", "show", "table", "1000"]).unwrap(),
            "100:\tfrom all lookup 1000\n"
        );
        assert_eq!(
            render_json(&snapshot, &["rule", "show", "lookup", "main"]),
            serde_json::json!([{"priority":32766,"src":"all","table":"main"}])
        );
        assert_eq!(
            render_json(&snapshot, &["rule", "show", "table", "100"]),
            serde_json::json!([])
        );
    }

    fn ifconfig_text(snapshot: &Snapshot, all: bool, short: bool) -> String {
        let mut output = Vec::new();
        render_ifconfig(snapshot, all, short, None, &mut output).unwrap();
        String::from_utf8(output).unwrap()
    }

    #[test]
    fn ifconfig_summary_filters_down_links_and_keeps_net_tools_flags_and_counters() {
        let mut stats = netlink_packet_route::link::Stats64::default();
        stats.rx_packets = 11;
        stats.tx_packets = 13;
        stats.rx_errors = 2;
        stats.tx_errors = 3;
        stats.rx_dropped = 5;
        stats.rx_missed_errors = 7;
        stats.tx_dropped = 19;
        stats.rx_fifo_errors = 17;
        stats.rx_over_errors = 23;
        stats.tx_fifo_errors = 29;
        let flags = (libc::IFF_UP
            | libc::IFF_RUNNING
            | libc::IFF_POINTOPOINT
            | libc::IFF_NOARP
            | libc::IFF_MULTICAST) as u32;
        let snapshot = snapshot(vec![
            link(3, "p10", flags, vec![LinkAttribute::Stats64(stats)]),
            link(2, "p2", flags, vec![LinkAttribute::Stats64(stats)]),
            link(4, "down", 0, vec![LinkAttribute::Stats64(stats)]),
        ]);
        assert_eq!(
            ifconfig_text(&snapshot, false, true),
            concat!(
                "p2               1500       11      2     12 17           13      3     19     29 MOPRU\n",
                "p10              1500       11      2     12 17           13      3     19     29 MOPRU\n"
            )
        );
        assert!(
            ifconfig_text(&snapshot, true, true)
                .lines()
                .next()
                .unwrap()
                .ends_with("[NO FLAGS]")
        );
    }

    #[test]
    fn ifconfig_point_to_point_hardware_and_error_counters_are_not_ethernet_guesses() {
        let mut stats = netlink_packet_route::link::Stats64::default();
        stats.rx_packets = 11;
        stats.tx_packets = 13;
        stats.rx_bytes = 2048;
        stats.tx_bytes = 1024;
        stats.rx_errors = 2;
        stats.tx_errors = 3;
        stats.rx_dropped = 5;
        stats.rx_missed_errors = 7;
        stats.rx_fifo_errors = 17;
        stats.rx_length_errors = 19;
        stats.rx_over_errors = 23;
        stats.rx_crc_errors = 29;
        stats.rx_frame_errors = 31;
        stats.tx_carrier_errors = 37;
        stats.tx_aborted_errors = 41;
        stats.tx_window_errors = 43;
        stats.tx_heartbeat_errors = 47;
        stats.tx_fifo_errors = 53;
        stats.collisions = 59;
        let flags = (libc::IFF_UP
            | libc::IFF_RUNNING
            | libc::IFF_POINTOPOINT
            | libc::IFF_NOARP
            | libc::IFF_MULTICAST) as u32;
        let mut message = link(
            2,
            "p2",
            flags,
            vec![
                LinkAttribute::Stats64(stats),
                LinkAttribute::Address(vec![]),
                LinkAttribute::TxQueueLen(500),
            ],
        );
        message.header.link_layer_type = LinkLayerType::None;
        let mut snapshot = snapshot(vec![message]);
        snapshot.addresses.push(address(
            2,
            "192.0.2.1",
            0,
            AddressFlags::Permanent,
            vec![
                AddressAttribute::Address("192.0.2.2".parse().unwrap()),
                AddressAttribute::Broadcast("192.0.2.255".parse().unwrap()),
            ],
        ));
        assert_eq!(
            ifconfig_text(&snapshot, false, false),
            concat!(
                "p2: flags=4305<UP,POINTOPOINT,RUNNING,NOARP,MULTICAST>  mtu 1500\n",
                "        inet 192.0.2.1  netmask 255.255.255.0  destination 192.0.2.2\n",
                "        unspec 00-00-00-00-00-00-00-00-00-00-00-00-00-00-00-00  txqueuelen 500  (UNSPEC)\n",
                "        RX packets 11  bytes 2048 (2.0 KiB)\n",
                "        RX errors 2  dropped 12  overruns 17  frame 102\n",
                "        TX packets 13  bytes 1024 (1024.0 B)\n",
                "        TX errors 3  dropped 0 overruns 53  carrier 168  collisions 59\n\n"
            )
        );
        snapshot.addresses[0].peer = None;
        assert!(ifconfig_text(&snapshot, false, false).contains("destination 192.0.2.1\n"));
        for (kind, expected) in [
            (
                LinkLayerType::Loopback,
                "        loop  txqueuelen 500  (Local Loopback)\n",
            ),
            (
                LinkLayerType::Tunnel,
                "        tunnel   txqueuelen 500  (IPIP Tunnel)\n",
            ),
            (
                LinkLayerType::Sit,
                "        sit  txqueuelen 500  (IPv6-in-IPv4)\n",
            ),
        ] {
            snapshot.links[0].link_type = kind.into();
            assert!(ifconfig_text(&snapshot, false, false).contains(expected));
        }
    }

    #[test]
    fn ifconfig_ipv6_text_follows_the_kernel_procfs_listing_order() {
        let mut snapshot = snapshot(vec![link(2, "p2", libc::IFF_UP as u32, vec![])]);
        snapshot.addresses = vec![
            address(2, "2001:db8::1", 0, AddressFlags::Permanent, vec![]),
            address(2, "2001:db8::2", 0, AddressFlags::Permanent, vec![]),
        ];
        order_ifconfig_ipv6(
            io::Cursor::new(concat!(
                "20010db8000000000000000000000002 02 40 00 80 p2\n",
                "20010db8000000000000000000000001 02 40 00 80 p2\n"
            )),
            &mut snapshot.addresses,
        )
        .unwrap();
        let output = ifconfig_text(&snapshot, false, false);
        assert_eq!(
            output
                .lines()
                .filter(|line| line.starts_with("        inet6 "))
                .collect::<Vec<_>>(),
            [
                "        inet6 2001:db8::2  prefixlen 64  scopeid 0x0<global>",
                "        inet6 2001:db8::1  prefixlen 64  scopeid 0x0<global>",
            ]
        );
    }

    fn calculator(args: &[&str]) -> Result<String, CommandError> {
        let args = std::iter::once("ipcalc")
            .chain(args.iter().copied())
            .map(OsString::from)
            .collect();
        let mut output = Vec::new();
        run_ipcalc(args, &mut output)?;
        Ok(String::from_utf8(output).unwrap())
    }

    #[test]
    fn ipcalc_human_output_and_host_only_operands_use_modern_not_perl_policy() {
        assert_eq!(
            calculator(&["10.1.2.3"]).unwrap(),
            "Address:\t10.1.2.3\nAddress space:\tPrivate Use\n"
        );
        assert_eq!(
            calculator(&["192.0.2.1/31"]).unwrap(),
            concat!(
                "Address:\t192.0.2.1\nNetwork:\t192.0.2.0/31\nNetmask:\t255.255.255.254 = 31\n",
                "Broadcast:\t255.255.255.255\n\nAddress space:\tDocumentation (TEST-NET-1)\n",
                "HostMin:\t192.0.2.0\nHostMax:\t192.0.2.1\nHosts/Net:\t2\n"
            )
        );
        assert_eq!(
            calculator(&["2001:db8::42/64"]).unwrap(),
            concat!(
                "Full Address:\t2001:0db8:0000:0000:0000:0000:0000:0042\nAddress:\t2001:db8::42\n",
                "Full Network:\t2001:0db8:0000:0000:0000:0000:0000:0000/64\nNetwork:\t2001:db8::/64\n",
                "Netmask:\tffff:ffff:ffff:ffff:: = 64\n\nAddress space:\tDocumentation\n",
                "HostMin:\t2001:db8::\nHostMax:\t2001:db8::ffff:ffff:ffff:ffff\n",
                "Hosts/Net:\t2^(64) = 18446744073709551616\n"
            )
        );
        assert_eq!(
            calculator(&["-bmpn", "2001:db8::42/64"]).unwrap(),
            "NETMASK=ffff:ffff:ffff:ffff::\nPREFIX=64\nNETWORK=2001:db8::\n"
        );
        assert_eq!(
            calculator(&["-nmp", "10.1.2.3"]).unwrap(),
            "NETMASK=255.255.255.255\nPREFIX=32\nNETWORK=10.1.2.3\n"
        );
        assert_eq!(
            calculator(&["-nm", "192.168.7.42", "255.255.255.0"]).unwrap(),
            "NETMASK=255.255.255.0\nNETWORK=192.168.7.0\n"
        );
    }

    #[test]
    fn ipcalc_json_preserves_exact_ipv6_count_and_single_host_semantics() {
        let value: serde_json::Value =
            serde_json::from_str(&calculator(&["-j", "::/0"]).unwrap()).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "FULLNETWORK":"0000:0000:0000:0000:0000:0000:0000:0000",
                "NETWORK":"::", "NETMASK":"::", "PREFIX":"0", "ADDRSPACE":"Reserved",
                "MINADDR":"::", "MAXADDR":"ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff",
                "ADDRESSES":"340282366920938463463374607431768211456"
            })
        );
        for address in ["192.0.2.1", "192.0.2.1/32"] {
            let value: serde_json::Value =
                serde_json::from_str(&calculator(&["-jn", address]).unwrap()).unwrap();
            assert_eq!(
                value,
                serde_json::json!({
                    "ADDRESS":"192.0.2.1", "NETWORK":"192.0.2.1", "NETMASK":"255.255.255.255",
                    "PREFIX":"32", "BROADCAST":"192.0.2.1", "MINADDR":"192.0.2.1", "MAXADDR":"192.0.2.1",
                    "ADDRSPACE":"Documentation (TEST-NET-1)", "ADDRESSES":"1"
                })
            );
        }
        for address in ["2001:db8::1", "2001:db8::1/128"] {
            let value: serde_json::Value =
                serde_json::from_str(&calculator(&["-j", address]).unwrap()).unwrap();
            assert_eq!(
                value,
                serde_json::json!({
                    "FULLADDRESS":"2001:0db8:0000:0000:0000:0000:0000:0001",
                    "ADDRESS":"2001:db8::1",
                    "FULLNETWORK":"2001:0db8:0000:0000:0000:0000:0000:0001",
                    "NETWORK":"2001:db8::1", "NETMASK":"ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff",
                    "PREFIX":"128", "MINADDR":"2001:db8::1", "MAXADDR":"2001:db8::1",
                    "ADDRSPACE":"Documentation", "ADDRESSES":"1"
                })
            );
        }
        for args in [
            vec!["192.0.2.1/24", "255.255.0.0"],
            vec!["192.0.2.1", "24", "garbage"],
            vec!["2001:db8::1", "64"],
            vec!["-4", "::1"],
            vec!["-6", "192.0.2.1"],
            vec!["-46", "::1"],
            vec!["-cn", "192.0.2.1"],
        ] {
            assert_eq!(calculator(&args).unwrap_err().code, 1, "{args:?}");
        }
    }
}
