use std::ffi::OsString;
use std::net::IpAddr;

use hickory_resolver::config::{NameServerConfig, ResolverConfig};
use hickory_resolver::net::runtime::TokioRuntimeProvider;
use hickory_resolver::proto::rr::RecordType;
use hickory_resolver::{Resolver, TokioResolver};

pub fn nslookup(args: Vec<OsString>) -> i32 {
    run("nslookup", args, Style::Nslookup)
}

pub fn host(args: Vec<OsString>) -> i32 {
    run("host", args, Style::Host)
}

#[derive(Clone, Copy)]
enum Style {
    Nslookup,
    Host,
}

fn run(program: &str, args: Vec<OsString>, style: Style) -> i32 {
    match parse(args, style).and_then(execute) {
        Ok(()) => 0,
        Err(error) if error.is_empty() => 0,
        Err(error) => {
            eprintln!("{program}: {error}");
            1
        }
    }
}

struct Options {
    style: Style,
    name: String,
    server: Option<String>,
    record_type: Option<RecordType>,
    verbose: bool,
}

fn parse(args: Vec<OsString>, style: Style) -> Result<Options, String> {
    let values = args
        .into_iter()
        .map(|value| {
            value.into_string().map_err(|value| {
                format!("argument is not valid UTF-8: {}", value.to_string_lossy())
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut name = None;
    let mut server = None;
    let mut record_type = None;
    let mut verbose = false;
    let mut iter = values.into_iter().skip(1);

    while let Some(value) = iter.next() {
        match value.as_str() {
            "-h" | "--help" => {
                match style {
                    Style::Nslookup => println!("Usage: nslookup [-type=TYPE] HOST [DNS_SERVER]"),
                    Style::Host => println!("Usage: host [-a] [-t TYPE] NAME [SERVER]"),
                }
                return Err(String::new());
            }
            "-a" | "-v" => verbose = true,
            "-t" | "--type" => {
                let value = iter
                    .next()
                    .ok_or("record type option requires an argument")?;
                record_type = Some(parse_record_type(&value)?);
            }
            value if value.starts_with("-type=") || value.starts_with("-query=") => {
                let value = value.split_once('=').expect("checked '='").1;
                record_type = Some(parse_record_type(value)?);
            }
            value if value.starts_with('-') => return Err(format!("unknown option '{value}'")),
            _ if name.is_none() => name = Some(value),
            _ if server.is_none() => server = Some(value),
            _ => return Err("too many arguments".into()),
        }
    }

    let name = name.ok_or("missing name")?;
    Ok(Options {
        style,
        name,
        server,
        record_type,
        verbose,
    })
}

fn execute(options: Options) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(execute_async(options))
}

async fn execute_async(options: Options) -> Result<(), String> {
    let resolver = if let Some(server) = &options.server {
        let address = resolve_server(server).await?;
        Resolver::builder_with_config(
            ResolverConfig::from_name_servers(vec![NameServerConfig::udp_and_tcp(address)]),
            TokioRuntimeProvider::default(),
        )
        .build()
        .map_err(|error| error.to_string())?
    } else {
        TokioResolver::builder_tokio()
            .map_err(|error| error.to_string())?
            .build()
            .map_err(|error| error.to_string())?
    };

    if let Ok(address) = options.name.parse::<IpAddr>() {
        let response = resolver
            .reverse_lookup(address)
            .await
            .map_err(|error| error.to_string())?;
        for record in response.answers() {
            match options.style {
                Style::Nslookup => println!("name = {}", record.data),
                Style::Host => println!(
                    "{} domain name pointer {}",
                    reverse_name(address),
                    record.data
                ),
            }
        }
        return Ok(());
    }

    if let Some(record_type) = options.record_type {
        return print_lookup(&resolver, &options, record_type).await;
    }

    match options.style {
        Style::Nslookup => {
            if let Some(server) = system_nameserver() {
                println!("Server:\t\t{server}\nAddress:\t{server}#53\n");
            }
            let response = resolver
                .lookup_ip(options.name.as_str())
                .await
                .map_err(|error| error.to_string())?;
            println!("Name:\t{}", options.name);
            for address in response.iter() {
                println!("Address: {address}");
            }
        }
        Style::Host => {
            let response = resolver
                .lookup_ip(options.name.as_str())
                .await
                .map_err(|error| error.to_string())?;
            for address in response.iter() {
                let family = if address.is_ipv4() {
                    "address"
                } else {
                    "IPv6 address"
                };
                println!("{} has {family} {address}", options.name);
            }
            if options.verbose {
                for record_type in [RecordType::MX, RecordType::NS, RecordType::SOA] {
                    let _ = print_lookup(&resolver, &options, record_type).await;
                }
            }
        }
    }
    Ok(())
}

async fn print_lookup(
    resolver: &TokioResolver,
    options: &Options,
    record_type: RecordType,
) -> Result<(), String> {
    let response = resolver
        .lookup(options.name.as_str(), record_type)
        .await
        .map_err(|error| error.to_string())?;
    for record in response.answers() {
        match options.style {
            Style::Nslookup => println!("{}\t{}", options.name, record.data),
            Style::Host => println!(
                "{} {} {}",
                options.name,
                record_label(record_type),
                record.data
            ),
        }
    }
    Ok(())
}

fn parse_record_type(value: &str) -> Result<RecordType, String> {
    match value.to_ascii_uppercase().as_str() {
        "A" => Ok(RecordType::A),
        "AAAA" => Ok(RecordType::AAAA),
        "ANY" => Ok(RecordType::ANY),
        "CNAME" => Ok(RecordType::CNAME),
        "MX" => Ok(RecordType::MX),
        "NS" => Ok(RecordType::NS),
        "PTR" => Ok(RecordType::PTR),
        "SOA" => Ok(RecordType::SOA),
        "SRV" => Ok(RecordType::SRV),
        "TXT" => Ok(RecordType::TXT),
        _ => Err(format!("unknown query type '{value}'")),
    }
}

fn record_label(record_type: RecordType) -> &'static str {
    match record_type {
        RecordType::A => "has address",
        RecordType::AAAA => "has IPv6 address",
        RecordType::CNAME => "is an alias for",
        RecordType::MX => "mail is handled by",
        RecordType::NS => "name server",
        RecordType::PTR => "domain name pointer",
        RecordType::SOA => "has SOA record",
        RecordType::SRV => "has SRV record",
        RecordType::TXT => "descriptive text",
        _ => "has record",
    }
}

async fn resolve_server(server: &str) -> Result<IpAddr, String> {
    if let Ok(address) = server.parse() {
        return Ok(address);
    }
    tokio::net::lookup_host((server, 53))
        .await
        .map_err(|error| error.to_string())?
        .next()
        .map(|address| address.ip())
        .ok_or_else(|| format!("{server}: no address"))
}

fn system_nameserver() -> Option<String> {
    std::fs::read_to_string("/etc/resolv.conf")
        .ok()?
        .lines()
        .find_map(|line| {
            let mut fields = line.split_whitespace();
            (fields.next() == Some("nameserver"))
                .then(|| fields.next().map(str::to_owned))
                .flatten()
        })
}

fn reverse_name(address: IpAddr) -> String {
    match address {
        IpAddr::V4(address) => {
            let [a, b, c, d] = address.octets();
            format!("{d}.{c}.{b}.{a}.in-addr.arpa")
        }
        IpAddr::V6(address) => {
            let mut digits = address
                .octets()
                .iter()
                .rev()
                .flat_map(|byte| [byte & 0x0f, byte >> 4])
                .map(|digit| format!("{digit:x}"))
                .collect::<Vec<_>>()
                .join(".");
            digits.push_str(".ip6.arpa");
            digits
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_types_are_case_insensitive() {
        assert_eq!(parse_record_type("mx").unwrap(), RecordType::MX);
        assert!(parse_record_type("bogus").is_err());
    }

    #[test]
    fn ipv4_reverse_name_matches_dns_convention() {
        assert_eq!(
            reverse_name("192.0.2.1".parse().unwrap()),
            "1.2.0.192.in-addr.arpa"
        );
    }
}
