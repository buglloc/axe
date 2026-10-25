use std::ffi::OsString;
use std::io::{self, Write};
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use hickory_resolver::config::{ResolverConfig, ResolverOpts};
use hickory_resolver::proto::op::{Message, MessageType, Query, ResponseCode};
use hickory_resolver::proto::rr::{Name, RData, Record, RecordType};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};

pub fn nslookup(args: Vec<OsString>) -> i32 {
    run("nslookup", args, Style::Nslookup)
}

pub fn host(args: Vec<OsString>) -> i32 {
    run("host", args, Style::Host)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Style {
    Nslookup,
    Host,
}

#[derive(Debug)]
struct ArgumentError {
    message: String,
    status: i32,
}

impl From<String> for ArgumentError {
    fn from(message: String) -> Self {
        Self { message, status: 1 }
    }
}

impl From<&str> for ArgumentError {
    fn from(message: &str) -> Self {
        message.to_owned().into()
    }
}

fn run(program: &str, args: Vec<OsString>, style: Style) -> i32 {
    let options = match parse(args, style) {
        Ok(Some(options)) => options,
        Ok(None) => return 0,
        Err(error) => {
            eprintln!("{program}: {}", error.message);
            return error.status;
        }
    };
    match execute(options) {
        Ok(true) => 0,
        Ok(false) => 1,
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
    timeout: Option<Duration>,
}

fn parse(args: Vec<OsString>, style: Style) -> Result<Option<Options>, ArgumentError> {
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
    let mut timeout = None;
    let mut iter = values.into_iter().skip(1);

    while let Some(value) = iter.next() {
        match value.as_str() {
            "-h" | "--help" => {
                match style {
                    Style::Nslookup => println!(
                        "Usage: nslookup [-timeout=SECONDS] [-type=TYPE] HOST [DNS_SERVER]"
                    ),
                    Style::Host => {
                        println!("Usage: host [-a] [-W SECONDS] [-t TYPE] NAME [SERVER]")
                    }
                }
                return Ok(None);
            }
            "-a" => {
                verbose = true;
                record_type = Some(RecordType::ANY);
            }
            "-v" => verbose = true,
            "-t" | "--type" => {
                let value = iter
                    .next()
                    .ok_or("record type option requires an argument")?;
                record_type = Some(parse_type_argument(&value, style)?);
            }
            "-W" | "-timeout" => {
                timeout = Some(parse_timeout(
                    &iter.next().ok_or("timeout requires an argument")?,
                )?);
            }
            value if value.starts_with("-type=") || value.starts_with("-query=") => {
                let (_, value) = value.split_once('=').expect("checked '='");
                record_type = Some(parse_type_argument(value, style)?);
            }
            value if value.starts_with("-timeout=") => {
                let (_, value) = value.split_once('=').expect("checked '='");
                timeout = Some(parse_timeout(value)?);
            }
            value if value.starts_with("-W") && value.len() > 2 => {
                timeout = Some(parse_timeout(&value[2..])?);
            }
            value if value.starts_with("-t") && value.len() > 2 => {
                record_type = Some(parse_type_argument(&value[2..], style)?);
            }
            value if value.starts_with('-') => {
                return Err(format!("unknown option '{value}'").into());
            }
            _ if name.is_none() => name = Some(value),
            _ if server.is_none() => server = Some(value),
            _ => return Err("too many arguments".into()),
        }
    }

    Ok(Some(Options {
        style,
        name: name.ok_or("missing name")?,
        server,
        record_type,
        verbose,
        timeout,
    }))
}

fn parse_type_argument(value: &str, style: Style) -> Result<RecordType, ArgumentError> {
    parse_record_type(value).map_err(|message| ArgumentError {
        message: if style == Style::Host {
            format!("invalid type: {value}")
        } else {
            message
        },
        status: if style == Style::Host { 2 } else { 1 },
    })
}

fn parse_timeout(value: &str) -> Result<Duration, String> {
    value
        .parse::<f64>()
        .ok()
        .and_then(|seconds| Duration::try_from_secs_f64(seconds).ok())
        .filter(|duration| !duration.is_zero() && Instant::now().checked_add(*duration).is_some())
        .ok_or_else(|| format!("invalid timeout: '{value}'"))
}

fn execute(options: Options) -> Result<bool, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(execute_async(options))
}

struct DnsResponse {
    message: Message,
    server: SocketAddr,
    wire_length: usize,
    elapsed: Duration,
}

struct DnsClient {
    servers: Vec<SocketAddr>,
    config: ResolverConfig,
    options: ResolverOpts,
}

impl DnsClient {
    async fn new(options: &Options) -> Result<Self, String> {
        let (config, mut resolver_options) = if options.server.is_some() {
            (ResolverConfig::default(), ResolverOpts::default())
        } else {
            hickory_resolver::system_conf::read_system_conf().map_err(|error| error.to_string())?
        };
        if let Some(timeout) = options.timeout {
            resolver_options.timeout = timeout;
        }
        let servers = if let Some(server) = &options.server {
            vec![SocketAddr::new(resolve_server(server).await?, 53)]
        } else {
            config
                .name_servers
                .iter()
                .map(|server| SocketAddr::new(server.ip, 53))
                .collect()
        };
        if servers.is_empty() {
            return Err("no DNS servers configured".into());
        }
        Ok(Self {
            servers,
            config,
            options: resolver_options,
        })
    }

    fn names(&self, value: &str) -> Result<Vec<Name>, String> {
        let name = Name::from_utf8(value).map_err(|error| error.to_string())?;
        if name.is_fqdn() {
            return Ok(vec![name]);
        }
        let mut absolute = name.clone();
        absolute.set_fqdn(true);
        let absolute_first = usize::from(name.num_labels().saturating_sub(1)) >= self.options.ndots;
        let mut names = Vec::with_capacity(self.config.search.len() + 2);
        if absolute_first {
            names.push(absolute.clone());
        }
        for suffix in self.config.search.iter().chain(self.config.domain.iter()) {
            if let Ok(candidate) = name.clone().append_domain(suffix)
                && !names.contains(&candidate)
            {
                names.push(candidate);
            }
        }
        if !names.contains(&absolute) {
            names.push(absolute);
        }
        Ok(names)
    }

    async fn lookup(&self, name: &Name, record_type: RecordType) -> Result<DnsResponse, String> {
        let mut query = Message::query();
        query.metadata.recursion_desired = self.options.recursion_desired;
        query.add_query(Query::query(name.clone(), record_type));
        let wire = query.to_vec().map_err(|error| error.to_string())?;
        let mut last_error = String::new();
        let mut last_response = None;
        for _ in 0..self.options.attempts.max(1) {
            for &server in &self.servers {
                match tokio::time::timeout(self.options.timeout, exchange(server, &query, &wire))
                    .await
                {
                    Ok(Ok(response))
                        if matches!(
                            response.message.metadata.response_code,
                            ResponseCode::ServFail | ResponseCode::Refused
                        ) =>
                    {
                        last_response = Some(response);
                    }
                    Ok(Ok(response)) => return Ok(response),
                    Ok(Err(error)) => {
                        last_error = format!(
                            "communications error to {}#{}: {error}",
                            server.ip(),
                            server.port()
                        )
                    }
                    Err(_) => {
                        last_error = format!(
                            "communications error to {}#{}: timed out",
                            server.ip(),
                            server.port()
                        )
                    }
                }
            }
        }
        last_response.ok_or(last_error)
    }
}

async fn exchange(server: SocketAddr, query: &Message, wire: &[u8]) -> io::Result<DnsResponse> {
    let started = Instant::now();
    let socket = UdpSocket::bind(if server.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    })
    .await?;
    socket.connect(server).await?;
    socket.send(wire).await?;
    let mut buffer = vec![0; 65_535];
    loop {
        let length = socket.recv(&mut buffer).await?;
        let Ok(message) = Message::from_vec(&buffer[..length]) else {
            continue;
        };
        if !response_matches(&message, query) {
            continue;
        }
        if !message.metadata.truncation {
            return Ok(DnsResponse {
                message,
                server,
                wire_length: length,
                elapsed: started.elapsed(),
            });
        }
        let mut stream = TcpStream::connect(server).await?;
        stream.write_u16(wire.len() as u16).await?;
        stream.write_all(wire).await?;
        let length = usize::from(stream.read_u16().await?);
        if length < 12 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "short DNS TCP response",
            ));
        }
        buffer.resize(length, 0);
        stream.read_exact(&mut buffer).await?;
        let message = Message::from_vec(&buffer)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if !response_matches(&message, query) || message.metadata.truncation {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "mismatched or truncated DNS TCP response",
            ));
        }
        return Ok(DnsResponse {
            message,
            server,
            wire_length: length,
            elapsed: started.elapsed(),
        });
    }
}

fn response_matches(response: &Message, query: &Message) -> bool {
    response.metadata.message_type == MessageType::Response
        && response.metadata.id == query.metadata.id
        && response.metadata.op_code == query.metadata.op_code
        && response.queries == query.queries
}

async fn execute_async(options: Options) -> Result<bool, String> {
    let client = DnsClient::new(&options).await?;
    let reverse = options.name.parse::<IpAddr>().ok();
    let query_name = reverse.map_or_else(|| options.name.clone(), reverse_name);
    let single_type = [if reverse.is_some() {
        RecordType::PTR
    } else {
        options.record_type.unwrap_or(RecordType::PTR)
    }];
    let default_types = [RecordType::A, RecordType::AAAA, RecordType::MX];
    let types = if options.record_type.is_some() || reverse.is_some() {
        &single_type[..]
    } else if options.style == Style::Host {
        &default_types[..]
    } else {
        &default_types[..2]
    };
    let candidates = client.names(&query_name)?;
    let mut first_response = None;
    for (index, candidate) in candidates.iter().enumerate() {
        let response = client.lookup(candidate, types[0]).await?;
        let try_next = response.message.metadata.response_code == ResponseCode::NXDomain
            || (response.message.metadata.response_code == ResponseCode::NoError
                && response.message.answers.is_empty());
        if !try_next || index + 1 == candidates.len() {
            first_response = Some(response);
            break;
        }
    }
    let mut response = first_response.ok_or("no DNS query names")?;
    let name = response.message.queries[0].name().clone();
    let followup_name = canonical_name(&response.message, &name);
    let first_had_answers = !response.message.answers.is_empty();
    let mut printed_non_authoritative = false;
    for (index, record_type) in types.iter().copied().enumerate() {
        if index > 0 {
            response = client.lookup(&followup_name, record_type).await?;
        }
        let name = response.message.queries[0].name().to_string();
        let mut output = io::stdout().lock();
        if options.verbose && options.style == Style::Host {
            writeln!(output, "Trying \"{}\"", display_name(&name))
                .map_err(|error| error.to_string())?;
        }
        if index == 0 {
            print_server(
                &mut output,
                &options,
                response.server,
                reverse.is_some() && options.record_type.is_none(),
            )
            .map_err(|error| error.to_string())?;
        }
        let code = response.message.metadata.response_code;
        if code != ResponseCode::NoError {
            print_dns_error(&mut output, options.style, &name, code)
                .map_err(|error| error.to_string())?;
            return Ok(false);
        }
        if options.verbose && options.style == Style::Host {
            print_verbose(&mut output, &response).map_err(|error| error.to_string())?;
        }
        if options.style == Style::Nslookup
            && !response.message.metadata.authoritative
            && !printed_non_authoritative
        {
            writeln!(output, "Non-authoritative answer:").map_err(|error| error.to_string())?;
            printed_non_authoritative = true;
        }
        if response.message.answers.is_empty() {
            if !options.verbose && options.style == Style::Host && options.record_type.is_some() {
                writeln!(
                    output,
                    "{} has no {record_type} record",
                    display_name(&name)
                )
                .map_err(|error| error.to_string())?;
            } else if options.style == Style::Nslookup
                && (types.len() == 1 || (index + 1 == types.len() && !first_had_answers))
            {
                writeln!(output, "*** Can't find {}: No answer", display_name(&name))
                    .map_err(|error| error.to_string())?;
            }
        } else if !(options.verbose && options.style == Style::Host) {
            for record in &response.message.answers {
                print_record(&mut output, options.style, record)
                    .map_err(|error| error.to_string())?;
            }
        }
        if options.style == Style::Nslookup
            && !response.message.metadata.authoritative
            && !matches!(record_type, RecordType::A | RecordType::AAAA)
        {
            writeln!(output, "\nAuthoritative answers can be found from:")
                .map_err(|error| error.to_string())?;
            for record in response
                .message
                .authorities
                .iter()
                .chain(&response.message.additionals)
            {
                match &record.data {
                    RData::A(_) => writeln!(
                        output,
                        "{}\tinternet address = {}",
                        display_name(&record.name.to_string()),
                        record.data
                    ),
                    RData::AAAA(_) => writeln!(
                        output,
                        "{}\thas AAAA address {}",
                        display_name(&record.name.to_string()),
                        record.data
                    ),
                    _ => print_record(&mut output, Style::Nslookup, record),
                }
                .map_err(|error| error.to_string())?;
            }
        }
    }
    if options.style == Style::Nslookup {
        writeln!(io::stdout().lock()).map_err(|error| error.to_string())?;
    }
    Ok(true)
}

fn canonical_name(message: &Message, name: &Name) -> Name {
    let mut name = name.clone();
    for _ in 0..message.answers.len() {
        let next = message.answers.iter().find_map(|record| {
            if record.name == name
                && let RData::CNAME(target) = &record.data
            {
                return Some(target.0.clone());
            }
            None
        });
        match next {
            Some(next) if next != name => name = next,
            _ => break,
        }
    }
    name
}

fn print_server(
    output: &mut impl Write,
    options: &Options,
    server: SocketAddr,
    reverse: bool,
) -> io::Result<()> {
    match options.style {
        Style::Nslookup if !reverse => writeln!(
            output,
            "Server:\t\t{}\nAddress:\t{}#{}\n",
            server.ip(),
            server.ip(),
            server.port()
        ),
        Style::Host if options.server.is_some() => writeln!(
            output,
            "Using domain server:\nName: {}\nAddress: {}#{}\nAliases: \n",
            options.server.as_deref().unwrap_or_default(),
            server.ip(),
            server.port()
        ),
        _ => Ok(()),
    }
}

fn print_dns_error(
    output: &mut impl Write,
    style: Style,
    name: &str,
    code: ResponseCode,
) -> io::Result<()> {
    let label = response_code_label(code);
    match style {
        Style::Nslookup => writeln!(
            output,
            "** server can't find {}: {label}\n",
            display_name(name)
        ),
        Style::Host => writeln!(
            output,
            "Host {name} not found: {}({label})",
            u16::from(code)
        ),
    }
}

fn display_name(name: &str) -> &str {
    if name == "." {
        return name;
    }
    let Some(without_root) = name.strip_suffix('.') else {
        return name;
    };
    let escapes = without_root
        .bytes()
        .rev()
        .take_while(|byte| *byte == b'\\')
        .count();
    if escapes % 2 == 0 { without_root } else { name }
}

fn response_code_label(code: ResponseCode) -> String {
    match u16::from(code) {
        0 => "NOERROR".into(),
        1 => "FORMERR".into(),
        2 => "SERVFAIL".into(),
        3 => "NXDOMAIN".into(),
        4 => "NOTIMP".into(),
        5 => "REFUSED".into(),
        6 => "YXDOMAIN".into(),
        7 => "YXRRSET".into(),
        8 => "NXRRSET".into(),
        9 => "NOTAUTH".into(),
        10 => "NOTZONE".into(),
        11 => "RESERVED11".into(),
        12 => "RESERVED12".into(),
        13 => "RESERVED13".into(),
        14 => "RESERVED14".into(),
        15 => "RESERVED15".into(),
        16 => "BADVERS".into(),
        value => format!("?{value}"),
    }
}

fn print_record(output: &mut impl Write, style: Style, record: &Record) -> io::Result<()> {
    let owner = record.name.to_string();
    let owner = display_name(&owner);
    if style == Style::Host {
        if let Some(label) = record_label(record.record_type()) {
            write!(output, "{owner} {label} ")?;
        } else {
            write!(output, "{owner} has {} record ", record.record_type())?;
        }
        print_rdata(output, &record.data)?;
        return writeln!(output);
    }
    match &record.data {
        RData::A(_) | RData::AAAA(_) => {
            writeln!(output, "Name:\t{owner}\nAddress: {}", record.data)
        }
        RData::CNAME(_) => writeln!(output, "{owner}\tcanonical name = {}", record.data),
        RData::PTR(_) => writeln!(output, "{owner}\tname = {}", record.data),
        RData::MX(_) => writeln!(output, "{owner}\tmail exchanger = {}", record.data),
        RData::NS(_) => writeln!(output, "{owner}\tnameserver = {}", record.data),
        RData::SOA(soa) => writeln!(
            output,
            "{owner}\n\torigin = {}\n\tmail addr = {}\n\tserial = {}\n\trefresh = {}\n\tretry = {}\n\texpire = {}\n\tminimum = {}",
            display_name(&soa.mname.to_string()),
            display_name(&soa.rname.to_string()),
            soa.serial,
            soa.refresh as u32,
            soa.retry as u32,
            soa.expire as u32,
            soa.minimum
        ),
        RData::SRV(_) => writeln!(output, "{owner}\tservice = {}", record.data),
        RData::TXT(_) => {
            write!(output, "{owner}\ttext = ")?;
            print_rdata(output, &record.data)?;
            writeln!(output)
        }
        _ => {
            write!(output, "{owner}\t{} = ", record.record_type())?;
            print_rdata(output, &record.data)?;
            writeln!(output)
        }
    }
}

fn print_rdata(output: &mut impl Write, data: &RData) -> io::Result<()> {
    if let RData::TXT(text) = data {
        for (index, part) in text.txt_data.iter().enumerate() {
            if index > 0 {
                write!(output, " ")?;
            }
            write!(output, "\"")?;
            for &byte in part.iter() {
                match byte {
                    b'"' | b'\\' => write!(output, "\\{}", char::from(byte))?,
                    0x20..=0x7e => write!(output, "{}", char::from(byte))?,
                    _ => write!(output, "\\{byte:03}")?,
                }
            }
            write!(output, "\"")?;
        }
        Ok(())
    } else {
        write!(output, "{data}")
    }
}

fn print_verbose(output: &mut impl Write, response: &DnsResponse) -> io::Result<()> {
    let message = &response.message;
    let header = &message.metadata;
    writeln!(
        output,
        ";; ->>HEADER<<- opcode: {}, status: {}, id: {}",
        header.op_code,
        response_code_label(header.response_code),
        header.id
    )?;
    write!(output, ";; flags:")?;
    for (present, flag) in [
        (header.message_type == MessageType::Response, "qr"),
        (header.authoritative, "aa"),
        (header.truncation, "tc"),
        (header.recursion_desired, "rd"),
        (header.recursion_available, "ra"),
        (header.authentic_data, "ad"),
        (header.checking_disabled, "cd"),
    ] {
        if present {
            write!(output, " {flag}")?;
        }
    }
    writeln!(
        output,
        "; QUERY: {}, ANSWER: {}, AUTHORITY: {}, ADDITIONAL: {}\n",
        message.queries.len(),
        message.answers.len(),
        message.authorities.len(),
        message.additionals.len()
    )?;
    writeln!(output, ";; QUESTION SECTION:")?;
    for query in &message.queries {
        let name = format!(";{}", query.name());
        padded_name(output, &name, 4)?;
        writeln!(output, "{}\t{}", query.query_class(), query.query_type())?;
    }
    for (title, records) in [
        ("ANSWER", &message.answers),
        ("AUTHORITY", &message.authorities),
        ("ADDITIONAL", &message.additionals),
    ] {
        if records.is_empty() {
            continue;
        }
        writeln!(output, "\n;; {title} SECTION:")?;
        for record in records {
            padded_name(output, &record.name.to_string(), 3)?;
            write!(
                output,
                "{}\t{}\t{}\t",
                record.ttl,
                record.dns_class,
                record.record_type()
            )?;
            print_rdata(output, &record.data)?;
            writeln!(output)?;
        }
    }
    writeln!(
        output,
        "\nReceived {} bytes from {}#{} in {} ms",
        response.wire_length,
        response.server.ip(),
        response.server.port(),
        response.elapsed.as_millis()
    )
}

fn padded_name(output: &mut impl Write, name: &str, stops: usize) -> io::Result<()> {
    write!(output, "{name}")?;
    for _ in 0..stops.saturating_sub(name.len() / 8).max(1) {
        write!(output, "\t")?;
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

fn record_label(record_type: RecordType) -> Option<&'static str> {
    Some(match record_type {
        RecordType::A => "has address",
        RecordType::AAAA => "has IPv6 address",
        RecordType::CNAME => "is an alias for",
        RecordType::MX => "mail is handled by",
        RecordType::NS => "name server",
        RecordType::PTR => "domain name pointer",
        RecordType::SOA => "has SOA record",
        RecordType::SRV => "has SRV record",
        RecordType::TXT => "descriptive text",
        _ => return None,
    })
}

async fn resolve_server(server: &str) -> Result<IpAddr, String> {
    if let Ok(address) = server.parse() {
        return Ok(address);
    }
    let resolver = hickory_resolver::TokioResolver::builder_tokio()
        .map_err(|error| error.to_string())?
        .build()
        .map_err(|error| error.to_string())?;
    resolver
        .lookup_ip(server)
        .await
        .map_err(|error| error.to_string())?
        .iter()
        .next()
        .ok_or_else(|| format!("{server}: no address"))
}

fn reverse_name(address: IpAddr) -> String {
    match address {
        IpAddr::V4(address) => {
            let [a, b, c, d] = address.octets();
            format!("{d}.{c}.{b}.{a}.in-addr.arpa.")
        }
        IpAddr::V6(address) => {
            use std::fmt::Write;
            let mut name = String::with_capacity(73);
            for byte in address.octets().iter().rev() {
                write!(name, "{:x}.{:x}.", byte & 0x0f, byte >> 4)
                    .expect("String writes are infallible");
            }
            name.push_str("ip6.arpa.");
            name
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
            "1.2.0.192.in-addr.arpa."
        );
    }

    #[test]
    fn dns_text_preserves_root_and_escaped_label_terminal_dots() {
        use hickory_resolver::proto::rr::rdata::A;

        for (name, displayed) in [(".", "."), (r"foo\..", r"foo\."), (r"foo\.", r"foo\.")] {
            let record = Record::from_rdata(
                Name::from_utf8(name).unwrap(),
                60,
                RData::A(A::new(192, 0, 2, 42)),
            );
            let mut output = Vec::new();
            print_record(&mut output, Style::Host, &record).unwrap();
            assert_eq!(
                String::from_utf8(output).unwrap(),
                format!("{displayed} has address 192.0.2.42\n")
            );
            let mut output = Vec::new();
            print_record(&mut output, Style::Nslookup, &record).unwrap();
            assert_eq!(
                String::from_utf8(output).unwrap(),
                format!("Name:\t{displayed}\nAddress: 192.0.2.42\n")
            );
            let mut output = Vec::new();
            print_dns_error(&mut output, Style::Nslookup, name, ResponseCode::NXDomain).unwrap();
            assert_eq!(
                String::from_utf8(output).unwrap(),
                format!("** server can't find {displayed}: NXDOMAIN\n\n")
            );
        }
    }

    #[tokio::test]
    async fn wire_lookup_ignores_unmatched_datagrams_and_keeps_cname_owners() {
        use hickory_resolver::proto::op::OpCode;
        use hickory_resolver::proto::rr::rdata::{A, CNAME};

        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let endpoint = server.local_addr().unwrap();
        let mut query = Message::query();
        query.add_query(Query::query(
            Name::from_utf8("alias.audit.test.").unwrap(),
            RecordType::A,
        ));
        let wire = query.to_vec().unwrap();
        let mut answer = Message::response(query.metadata.id, OpCode::Query);
        answer.add_query(query.queries[0].clone());
        answer.add_answers([
            Record::from_rdata(
                Name::from_utf8("alias.audit.test.").unwrap(),
                60,
                RData::CNAME(CNAME(Name::from_utf8("audit.test.").unwrap())),
            ),
            Record::from_rdata(
                Name::from_utf8("audit.test.").unwrap(),
                60,
                RData::A(A::new(192, 0, 2, 42)),
            ),
        ]);
        let expected = answer.to_vec().unwrap();
        let fixture = async {
            let mut buffer = [0; 512];
            let (_, peer) = server.recv_from(&mut buffer).await.unwrap();
            server.send_to(&[0xff; 3], peer).await.unwrap();
            let mut wrong_id = answer.clone();
            wrong_id.metadata.id = wrong_id.metadata.id.wrapping_add(1);
            server
                .send_to(&wrong_id.to_vec().unwrap(), peer)
                .await
                .unwrap();
            let mut wrong_question = answer.clone();
            wrong_question.queries = vec![Query::query(
                Name::from_utf8("other.audit.test.").unwrap(),
                RecordType::A,
            )];
            server
                .send_to(&wrong_question.to_vec().unwrap(), peer)
                .await
                .unwrap();
            server.send_to(&expected, peer).await.unwrap();
        };
        let (response, ()) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(exchange(endpoint, &query, &wire), fixture)
        })
        .await
        .unwrap();
        let response = response.unwrap();
        assert_eq!(response.server, endpoint);
        assert_eq!(response.wire_length, expected.len());
        let mut output = Vec::new();
        for record in &response.message.answers {
            print_record(&mut output, Style::Nslookup, record).unwrap();
        }
        assert_eq!(output, b"alias.audit.test\tcanonical name = audit.test.\nName:\taudit.test\nAddress: 192.0.2.42\n");
    }

    #[tokio::test]
    async fn truncated_dns_response_uses_tcp_at_the_same_endpoint() {
        use hickory_resolver::proto::op::OpCode;
        use hickory_resolver::proto::rr::rdata::MX;

        let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let endpoint = udp.local_addr().unwrap();
        let tcp = tokio::net::TcpListener::bind(endpoint).await.unwrap();
        let mut query = Message::query();
        query.add_query(Query::query(
            Name::from_utf8("audit.test.").unwrap(),
            RecordType::MX,
        ));
        let wire = query.to_vec().unwrap();
        let mut truncated = Message::response(query.metadata.id, OpCode::Query);
        truncated.add_query(query.queries[0].clone());
        truncated.metadata.truncation = true;
        let mut answer = truncated.clone();
        answer.metadata.truncation = false;
        answer.add_answer(Record::from_rdata(
            Name::from_utf8("audit.test.").unwrap(),
            60,
            RData::MX(MX::new(10, Name::from_utf8("mail.audit.test.").unwrap())),
        ));
        let answer = answer.to_vec().unwrap();
        let fixture = async {
            let mut buffer = [0; 512];
            let (_, peer) = udp.recv_from(&mut buffer).await.unwrap();
            udp.send_to(&truncated.to_vec().unwrap(), peer)
                .await
                .unwrap();
            let (mut stream, _) = tcp.accept().await.unwrap();
            let length = usize::from(stream.read_u16().await.unwrap());
            let mut request = vec![0; length];
            stream.read_exact(&mut request).await.unwrap();
            assert_eq!(request, wire);
            stream.write_u16(answer.len() as u16).await.unwrap();
            stream.write_all(&answer).await.unwrap();
        };
        let (response, ()) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(exchange(endpoint, &query, &wire), fixture)
        })
        .await
        .unwrap();
        let response = response.unwrap();
        assert_eq!(response.server, endpoint);
        assert_eq!(response.wire_length, answer.len());
        let mut output = Vec::new();
        print_record(&mut output, Style::Nslookup, &response.message.answers[0]).unwrap();
        assert_eq!(
            output,
            b"audit.test\tmail exchanger = 10 mail.audit.test.\n"
        );
    }
}
