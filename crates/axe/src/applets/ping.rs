use std::ffi::OsString;
use std::net::IpAddr;
use std::time::{Duration, Instant};

use surge_ping::{Client, Config, ICMP, PingIdentifier, PingSequence};

pub fn ping(args: Vec<OsString>) -> i32 {
    run_ping(args, false)
}

pub fn ping6(args: Vec<OsString>) -> i32 {
    run_ping(args, true)
}

pub fn traceroute(args: Vec<OsString>) -> i32 {
    run_traceroute(args, false)
}

pub fn traceroute6(args: Vec<OsString>) -> i32 {
    run_traceroute(args, true)
}

struct PingOptions {
    host: String,
    force_v6: bool,
    count: Option<u16>,
    interval: Duration,
    timeout: Duration,
    size: usize,
    quiet: bool,
    numeric: bool,
    interface: Option<String>,
}

fn run_ping(args: Vec<OsString>, force_v6: bool) -> i32 {
    let options = match parse_ping(args, force_v6) {
        Ok(Some(options)) => options,
        Ok(None) => return 0,
        Err(error) => {
            eprintln!("ping: {error}");
            return 2;
        }
    };

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("ping: {error}");
            return 2;
        }
    };
    match runtime.block_on(ping_async(options)) {
        Ok(true) => 0,
        Ok(false) => 1,
        Err(error) => {
            eprintln!("ping: {error}");
            2
        }
    }
}

fn parse_ping(args: Vec<OsString>, force_v6: bool) -> Result<Option<PingOptions>, String> {
    let mut force_v6 = force_v6;
    let values = utf8_args(args)?;
    let mut count = None;
    let mut interval = Duration::from_secs(1);
    let mut timeout = Duration::from_secs(2);
    let mut size = 56;
    let mut quiet = false;
    let mut numeric = false;
    let mut interface = None;
    let mut host = None;
    let mut iter = values.into_iter().skip(1);
    while let Some(value) = iter.next() {
        match value.as_str() {
            "-4" if force_v6 => return Err("-4 conflicts with ping6".into()),
            "-4" => force_v6 = false,
            "-6" => force_v6 = true,
            "-c" | "--count" => count = Some(parse_value(iter.next(), "count")?),
            "-i" | "--interval" => {
                interval = Duration::from_secs_f64(parse_value(iter.next(), "interval")?)
            }
            "-W" | "--timeout" => {
                timeout = Duration::from_secs_f64(parse_value(iter.next(), "timeout")?)
            }
            "-s" | "--size" => size = parse_value(iter.next(), "size")?,
            "-I" | "--interface" => {
                interface = Some(iter.next().ok_or("interface requires an argument")?)
            }
            "-q" | "--quiet" => quiet = true,
            "-n" | "--numeric" => numeric = true,
            "-h" | "--help" => {
                println!("Usage: ping [-46q] [-c COUNT] [-i INTERVAL] [-W TIMEOUT] [-s SIZE] HOST");
                return Ok(None);
            }
            value if value.starts_with('-') => return Err(format!("unknown option '{value}'")),
            _ if host.is_none() => host = Some(value),
            _ => return Err("too many hosts".into()),
        }
    }
    let host = host.ok_or("missing host operand")?;
    Ok(Some(PingOptions {
        host,
        force_v6,
        count,
        interval,
        timeout,
        size,
        quiet,
        numeric,
        interface,
    }))
}

async fn ping_async(options: PingOptions) -> Result<bool, String> {
    let destination = resolve(&options.host, options.force_v6).await?;
    let kind = if destination.is_ipv6() {
        ICMP::V6
    } else {
        ICMP::V4
    };
    let mut builder = Config::builder().kind(kind);
    if let Some(interface) = &options.interface {
        builder = builder.interface(interface);
    }
    let client = Client::new(&builder.build()).map_err(|error| error.to_string())?;
    let identifier = PingIdentifier(std::process::id() as u16);
    let mut pinger = client.pinger(destination, identifier).await;
    pinger.timeout(options.timeout);
    let payload = vec![0x61; options.size];

    if !options.quiet {
        println!(
            "PING {} ({destination}) {}({}) bytes of data.",
            options.host,
            options.size,
            options.size + if destination.is_ipv4() { 28 } else { 48 }
        );
    }

    let started = Instant::now();
    let mut transmitted = 0u16;
    let mut received = 0u16;
    let mut total_rtt = Duration::ZERO;
    let mut minimum = None::<Duration>;
    let mut maximum = Duration::ZERO;
    loop {
        if options.count.is_some_and(|count| transmitted >= count) {
            break;
        }
        let sequence = transmitted;
        transmitted = transmitted.saturating_add(1);
        match pinger.ping(PingSequence(sequence), &payload).await {
            Ok((_packet, rtt)) => {
                received = received.saturating_add(1);
                total_rtt += rtt;
                minimum = Some(minimum.map_or(rtt, |value| value.min(rtt)));
                maximum = maximum.max(rtt);
                if !options.quiet {
                    let display_host = if options.numeric {
                        destination.to_string()
                    } else {
                        options.host.clone()
                    };
                    println!(
                        "{} bytes from {display_host} ({destination}): icmp_seq={} ttl={} time={:.3} ms",
                        options.size + 8,
                        sequence + 1,
                        64,
                        rtt.as_secs_f64() * 1000.0
                    );
                }
            }
            Err(error) => {
                if !options.quiet {
                    eprintln!("From {destination} icmp_seq={} {error}", sequence + 1);
                }
            }
        }

        if options.count.is_some_and(|count| transmitted >= count) {
            break;
        }
        if options.count.is_none() {
            tokio::select! {
                _ = tokio::time::sleep(options.interval) => {}
                signal = tokio::signal::ctrl_c() => {
                    signal.map_err(|error| error.to_string())?;
                    break;
                }
            }
        } else {
            tokio::time::sleep(options.interval).await;
        }
    }

    let loss = if transmitted == 0 {
        0.0
    } else {
        f64::from(transmitted - received) * 100.0 / f64::from(transmitted)
    };
    println!("\n--- {} ping statistics ---", options.host);
    println!(
        "{transmitted} packets transmitted, {received} received, {loss:.0}% packet loss, time {:.0}ms",
        started.elapsed().as_secs_f64() * 1000.0
    );
    if received > 0 {
        let average = total_rtt.as_secs_f64() * 1000.0 / f64::from(received);
        println!(
            "rtt min/avg/max = {:.3}/{average:.3}/{:.3} ms",
            minimum.unwrap_or_default().as_secs_f64() * 1000.0,
            maximum.as_secs_f64() * 1000.0
        );
    }
    Ok(received > 0)
}

struct TraceOptions {
    host: String,
    force_v6: bool,
    max_hops: u8,
    first_hop: u8,
    probes: u8,
    timeout: Duration,
    numeric: bool,
}

fn run_traceroute(args: Vec<OsString>, force_v6: bool) -> i32 {
    let options = match parse_trace(args, force_v6) {
        Ok(Some(options)) => options,
        Ok(None) => return 0,
        Err(error) => {
            eprintln!("traceroute: {error}");
            return 2;
        }
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("traceroute: {error}");
            return 1;
        }
    };
    match runtime.block_on(trace_async(options)) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("traceroute: {error}");
            1
        }
    }
}

fn parse_trace(args: Vec<OsString>, force_v6: bool) -> Result<Option<TraceOptions>, String> {
    let mut force_v6 = force_v6;
    let values = utf8_args(args)?;
    let mut max_hops = 30;
    let mut first_hop = 1;
    let mut probes = 3;
    let mut timeout = Duration::from_secs(5);
    let mut numeric = false;
    let mut host = None;
    let mut iter = values.into_iter().skip(1);
    while let Some(value) = iter.next() {
        match value.as_str() {
            "-4" if force_v6 => return Err("-4 conflicts with traceroute6".into()),
            "-4" => force_v6 = false,
            "-6" => force_v6 = true,
            "-I" => {}
            "-m" | "--max-hops" => max_hops = parse_value(iter.next(), "max hops")?,
            "-f" | "--first" => first_hop = parse_value(iter.next(), "first hop")?,
            "-q" | "--queries" => probes = parse_value(iter.next(), "queries")?,
            "-w" | "--wait" => timeout = Duration::from_secs_f64(parse_value(iter.next(), "wait")?),
            "-n" => numeric = true,
            "-h" | "--help" => {
                println!(
                    "Usage: traceroute [-46In] [-m MAX_HOPS] [-f FIRST_HOP] [-q PROBES] [-w WAIT] HOST"
                );
                return Ok(None);
            }
            value if value.starts_with('-') => return Err(format!("unknown option '{value}'")),
            _ if host.is_none() => host = Some(value),
            _ => return Err("too many hosts".into()),
        }
    }
    let host = host.ok_or("missing host operand")?;
    if first_hop == 0 || max_hops < first_hop || probes == 0 {
        return Err("invalid hop or probe count".into());
    }
    Ok(Some(TraceOptions {
        host,
        force_v6,
        max_hops,
        first_hop,
        probes,
        timeout,
        numeric,
    }))
}

async fn trace_async(options: TraceOptions) -> Result<(), String> {
    let destination = resolve(&options.host, options.force_v6).await?;
    println!(
        "traceroute to {} ({destination}), {} hops max, 60 byte packets",
        options.host, options.max_hops
    );
    for ttl in options.first_hop..=options.max_hops {
        print!(" {ttl:2} ");
        let mut reached = false;
        for probe in 0..options.probes {
            let kind = if destination.is_ipv6() {
                ICMP::V6
            } else {
                ICMP::V4
            };
            let config = Config::builder().kind(kind).ttl(u32::from(ttl)).build();
            let client = Client::new(&config).map_err(|error| error.to_string())?;
            let mut pinger = client
                .pinger(destination, PingIdentifier(std::process::id() as u16))
                .await;
            pinger.timeout(options.timeout);
            match pinger.ping(PingSequence(u16::from(probe)), &[0; 32]).await {
                Ok((_packet, elapsed)) => {
                    if !reached {
                        let name = if options.numeric {
                            destination.to_string()
                        } else {
                            format!("{} ({destination})", options.host)
                        };
                        print!("{name}  ");
                    }
                    print!("{:.3} ms  ", elapsed.as_secs_f64() * 1000.0);
                    reached = true;
                }
                Err(_) => print!("* "),
            }
        }
        println!();
        if reached {
            break;
        }
    }
    Ok(())
}

async fn resolve(host: &str, force_v6: bool) -> Result<IpAddr, String> {
    if let Ok(address) = host.parse::<IpAddr>() {
        if force_v6 && address.is_ipv4() {
            return Err("IPv4 address supplied to IPv6 command".into());
        }
        return Ok(address);
    }
    tokio::net::lookup_host((host, 0))
        .await
        .map_err(|error| error.to_string())?
        .map(|address| address.ip())
        .find(|address| !force_v6 || address.is_ipv6())
        .ok_or_else(|| format!("{host}: Name or service not known"))
}

fn parse_value<T: std::str::FromStr>(value: Option<String>, name: &str) -> Result<T, String> {
    let value = value.ok_or_else(|| format!("{name} requires an argument"))?;
    value
        .parse()
        .map_err(|_| format!("invalid {name}: '{value}'"))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_requires_a_host() {
        assert!(parse_ping(vec!["ping".into()], false).is_err());
    }

    #[test]
    fn traceroute_rejects_zero_probes() {
        assert!(
            parse_trace(
                vec![
                    "traceroute".into(),
                    "-q".into(),
                    "0".into(),
                    "localhost".into()
                ],
                false
            )
            .is_err()
        );
    }
}
