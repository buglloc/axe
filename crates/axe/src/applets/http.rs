use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Read, Write};
use std::num::NonZeroU64;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use base64::Engine;
use clap::Parser;
use clap::error::ErrorKind;
use serde::Serialize;
use ureq::http::{
    HeaderName, HeaderValue, Method, Request, Response, StatusCode, header::LOCATION,
};
use ureq::tls::{RootCerts, TlsConfig, TlsProvider};
use ureq::{Agent, AsSendBody, ResponseExt};

#[derive(Parser)]
#[command(
    name = "http",
    about = "Make bounded HTTP requests with structured output",
    disable_version_flag = true
)]
struct Options {
    /// HTTP method; defaults to GET, or POST when a request body is supplied
    #[arg(short = 'X', long, value_name = "METHOD")]
    method: Option<Method>,

    /// Add a request header as NAME:VALUE
    #[arg(short = 'H', long = "header", value_name = "NAME:VALUE")]
    headers: Vec<HeaderSpec>,

    /// Send a UTF-8 request body
    #[arg(short = 'd', long, value_name = "TEXT", conflicts_with = "data_file")]
    data: Option<String>,

    /// Stream the request body from PATH; use - for standard input
    #[arg(long, value_name = "PATH", conflicts_with = "data")]
    data_file: Option<PathBuf>,

    /// Bound the complete request, including redirects, in seconds
    #[arg(long, default_value = "30", value_name = "SECONDS")]
    timeout: NonZeroU64,

    /// Refuse response bodies larger than this many bytes
    #[arg(long, default_value = "16777216", value_name = "BYTES")]
    max_bytes: NonZeroU64,

    /// Follow redirects, up to a fixed limit of 10
    #[arg(short = 'L', long)]
    follow: bool,

    /// Use an explicit HTTP CONNECT proxy instead of proxy environment variables
    #[arg(long, value_name = "URL")]
    proxy: Option<String>,

    /// Disable TLS certificate and hostname verification
    #[arg(short = 'k', long)]
    insecure: bool,

    /// Write the response body directly instead of an axe_http JSON document
    #[arg(long)]
    body: bool,

    /// HTTP or HTTPS URL
    url: String,
}

#[derive(Clone)]
struct HeaderSpec {
    name: HeaderName,
    value: HeaderValue,
}

impl FromStr for HeaderSpec {
    type Err = String;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let (name, value) = input
            .split_once(':')
            .ok_or_else(|| "header must use NAME:VALUE syntax".to_string())?;
        let name = HeaderName::from_bytes(name.trim().as_bytes())
            .map_err(|error| format!("invalid header name: {error}"))?;
        let value = HeaderValue::from_str(value.trim_start())
            .map_err(|error| format!("invalid header value: {error}"))?;

        Ok(Self { name, value })
    }
}

#[derive(Serialize)]
struct HttpDocument {
    schema: &'static str,
    schema_version: u32,
    request: RequestRecord,
    response: ResponseRecord,
}

#[derive(Serialize)]
struct RequestRecord {
    method: String,
    url: String,
}

#[derive(Serialize)]
struct ResponseRecord {
    #[serde(flatten)]
    metadata: ResponseMetadata,
    body: EncodedBytes,
}

#[derive(Serialize)]
struct ResponseMetadata {
    url: String,
    status: u16,
    version: String,
    headers: Vec<HeaderRecord>,
    redirects: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resolved_location: Option<String>,
}

#[derive(Serialize)]
struct HeaderRecord {
    name: String,
    #[serde(flatten)]
    value: EncodedBytes,
}

#[derive(Serialize)]
struct EncodedBytes {
    encoding: &'static str,
    data: String,
}

#[derive(Serialize)]
struct ErrorDocument<'a> {
    schema: &'static str,
    schema_version: u32,
    request: RequestRecord,
    #[serde(skip_serializing_if = "Option::is_none")]
    response: Option<&'a ResponseMetadata>,
    error: ErrorRecord<'a>,
}

#[derive(Serialize)]
struct ErrorRecord<'a> {
    kind: &'a str,
    message: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    received_at_least_bytes: Option<u64>,
}

struct Failure {
    kind: &'static str,
    message: String,
    response: Option<Box<ResponseMetadata>>,
    limit_bytes: Option<u64>,
    received_at_least_bytes: Option<u64>,
}

impl Failure {
    fn new(kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            response: None,
            limit_bytes: None,
            received_at_least_bytes: None,
        }
    }

    fn response_too_large(limit_bytes: u64) -> Self {
        let received_at_least_bytes = limit_bytes.saturating_add(1);
        Self {
            kind: "response_too_large",
            message: format!(
                "response exceeded {limit_bytes}-byte limit (received at least {received_at_least_bytes} bytes)"
            ),
            response: None,
            limit_bytes: Some(limit_bytes),
            received_at_least_bytes: Some(received_at_least_bytes),
        }
    }

    fn with_response(mut self, response: ResponseMetadata) -> Self {
        self.response = Some(Box::new(response));
        self
    }

    fn from_ureq(error: ureq::Error) -> Self {
        if matches!(&error, ureq::Error::RedirectFailed) {
            return Self::new(
                "redirect_replay_unsupported",
                "307/308 redirect cannot replay this request method/body",
            );
        }
        let kind = match error {
            ureq::Error::BadUri(_) | ureq::Error::Http(_) => "invalid_request",
            ureq::Error::HostNotFound => "dns",
            ureq::Error::Timeout(_) => "timeout",
            ureq::Error::Tls(_) | ureq::Error::Rustls(_) | ureq::Error::TlsRequired => "tls",
            ureq::Error::TooManyRedirects => "redirect",
            ureq::Error::BodyExceedsLimit(_) => "response_body",
            _ => "transport",
        };

        Self::new(kind, error.to_string())
    }
}

pub fn http(args: Vec<OsString>) -> i32 {
    let options = match Options::try_parse_from(args) {
        Ok(options) => options,
        Err(error) => {
            let code = if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) {
                0
            } else {
                2
            };
            let _ = error.print();
            return code;
        }
    };

    let method = options.method.clone().unwrap_or_else(|| {
        if options.data.is_some() || options.data_file.is_some() {
            Method::POST
        } else {
            Method::GET
        }
    });
    let request = RequestRecord {
        method: method.to_string(),
        url: redact_url_password(&options.url),
    };

    match execute(&options, &method) {
        Ok(response) => {
            if let Some(document) = response
                && let Err(error) = write_json(&document)
            {
                eprintln!("http: write response: {error}");
                return 1;
            }
            0
        }
        Err(failure) => {
            if options.body {
                eprintln!("http: {}: {}", failure.kind, failure.message);
            } else {
                let document = ErrorDocument {
                    schema: "axe_http",
                    schema_version: 1,
                    request,
                    response: failure.response.as_deref(),
                    error: ErrorRecord {
                        kind: failure.kind,
                        message: &failure.message,
                        limit_bytes: failure.limit_bytes,
                        received_at_least_bytes: failure.received_at_least_bytes,
                    },
                };
                if let Err(error) = write_json(&document) {
                    eprintln!("http: {}: {}", failure.kind, failure.message);
                    eprintln!("http: write error document: {error}");
                }
            }
            1
        }
    }
}

fn execute(options: &Options, method: &Method) -> Result<Option<HttpDocument>, Failure> {
    if !is_supported_method(method) {
        return Err(Failure::new(
            "unsupported_method",
            format!(
                "{method} is not supported by the HTTP/1.1 backend; use GET, HEAD, POST, PUT, DELETE, CONNECT, OPTIONS, TRACE, or PATCH"
            ),
        ));
    }

    let agent = build_agent(options)?;
    let mut response = match (&options.data, &options.data_file) {
        (Some(data), None) => send(&agent, options, method, data.as_str()),
        (None, Some(path)) if path.as_os_str() == "-" => send(&agent, options, method, io::stdin()),
        (None, Some(path)) => {
            let file = File::open(path).map_err(|error| {
                Failure::new("input", format!("open {}: {error}", path.display()))
            })?;
            send(&agent, options, method, file)
        }
        (None, None) => send(&agent, options, method, ()),
        (Some(_), Some(_)) => unreachable!("clap rejects conflicting request bodies"),
    }?;
    let status = response.status();

    if options.body {
        let mut reader = response.body_mut().as_reader();
        let stdout = io::stdout();
        let mut output = stdout.lock();
        copy_limited(&mut reader, &mut output, options.max_bytes.get())?;
        output
            .flush()
            .map_err(|error| Failure::new("output", error.to_string()))?;

        return Ok(None);
    }

    let response_uri = response.get_uri().to_string();
    let resolved_location = if status.is_redirection() && status != StatusCode::NOT_MODIFIED {
        response
            .headers()
            .get(LOCATION)
            .and_then(|location| location.to_str().ok())
            .and_then(|location| {
                let base = url::Url::parse(&response_uri).ok()?;
                base.join(location).ok()
            })
            .map(|location| redact_url_password(location.as_str()))
    } else {
        None
    };
    let response_url = redact_url_password(&response_uri);
    let mut redirects = response
        .get_redirect_history()
        .unwrap_or_default()
        .iter()
        .map(|url| redact_url_password(&url.to_string()))
        .collect::<Vec<_>>();
    if redirects.last() == Some(&response_url) {
        redirects.pop();
    }
    let metadata = ResponseMetadata {
        url: response_url,
        status: status.as_u16(),
        version: format!("{:?}", response.version()),
        headers: response
            .headers()
            .iter()
            .map(|(name, value)| HeaderRecord {
                name: name.to_string(),
                value: encode_bytes(value.as_bytes()),
            })
            .collect(),
        redirects,
        resolved_location,
    };
    let max_bytes = options.max_bytes.get();
    let body = {
        let mut reader = response.body_mut().as_reader();
        match read_limited(&mut reader, max_bytes) {
            Ok(body) => body,
            Err(error) => return Err(error.with_response(metadata)),
        }
    };

    Ok(Some(HttpDocument {
        schema: "axe_http",
        schema_version: 1,
        request: RequestRecord {
            method: method.to_string(),
            url: redact_url_password(&options.url),
        },
        response: ResponseRecord {
            metadata,
            body: encode_bytes(&body),
        },
    }))
}

fn build_agent(options: &Options) -> Result<Agent, Failure> {
    let certificates = crate::tls_roots::certificates()
        .iter()
        .map(|certificate| ureq::tls::Certificate::from_der(certificate.as_ref()).to_owned())
        .collect::<Vec<_>>();
    let tls = TlsConfig::builder()
        .provider(TlsProvider::Rustls)
        .root_certs(RootCerts::from(certificates))
        .disable_verification(options.insecure)
        .build();

    let mut builder = Agent::config_builder()
        .tls_config(tls)
        .timeout_global(Some(Duration::from_secs(options.timeout.get())))
        .http_status_as_error(false)
        .max_redirects(if options.follow { 10 } else { 0 })
        .save_redirect_history(true)
        .user_agent(concat!("axe-http/", env!("CARGO_PKG_VERSION")));
    if let Some(proxy) = &options.proxy {
        let proxy = ureq::Proxy::new(proxy).map_err(Failure::from_ureq)?;
        builder = builder.proxy(Some(proxy));
    }

    Ok(builder.build().into())
}

fn send<B: AsSendBody>(
    agent: &Agent,
    options: &Options,
    method: &Method,
    body: B,
) -> Result<Response<ureq::Body>, Failure> {
    let mut request = Request::builder().method(method.clone()).uri(&options.url);
    for header in &options.headers {
        request = request.header(header.name.clone(), header.value.clone());
    }
    let request = request
        .body(body)
        .map_err(|error| Failure::new("invalid_request", error.to_string()))?;

    agent.run(request).map_err(Failure::from_ureq)
}

fn encode_bytes(bytes: &[u8]) -> EncodedBytes {
    match std::str::from_utf8(bytes) {
        Ok(text) => EncodedBytes {
            encoding: "utf8",
            data: text.to_owned(),
        },
        Err(_) => EncodedBytes {
            encoding: "base64",
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
        },
    }
}

fn is_supported_method(method: &Method) -> bool {
    matches!(
        *method,
        Method::GET
            | Method::HEAD
            | Method::POST
            | Method::PUT
            | Method::DELETE
            | Method::CONNECT
            | Method::OPTIONS
            | Method::TRACE
            | Method::PATCH
    )
}

fn redact_url_password(url: &str) -> String {
    let Some(scheme_end) = url.find("://") else {
        return url.to_owned();
    };
    let authority_start = scheme_end + 3;
    let authority_end = url[authority_start..]
        .find(['/', '?', '#'])
        .map_or(url.len(), |offset| authority_start + offset);
    let authority = &url[authority_start..authority_end];
    let Some(userinfo_end) = authority.rfind('@') else {
        return url.to_owned();
    };
    let userinfo = &authority[..userinfo_end];
    let Some(password_start) = userinfo.find(':') else {
        return url.to_owned();
    };

    let mut redacted = String::with_capacity(url.len());
    redacted.push_str(&url[..authority_start]);
    redacted.push_str(&userinfo[..password_start]);
    redacted.push_str(":[REDACTED]@");
    redacted.push_str(&authority[userinfo_end + 1..]);
    redacted.push_str(&url[authority_end..]);
    redacted
}

fn read_limited(reader: &mut impl Read, max_bytes: u64) -> Result<Vec<u8>, Failure> {
    let mut remaining = max_bytes;
    let mut body = Vec::new();
    let mut buffer = [0_u8; 32 * 1024];

    loop {
        if remaining == 0 {
            let mut extra = [0_u8; 1];
            let read = reader
                .read(&mut extra)
                .map_err(|error| Failure::new("response_body", error.to_string()))?;
            if read == 0 {
                return Ok(body);
            }
            return Err(Failure::response_too_large(max_bytes));
        }

        let capacity = usize::try_from(remaining)
            .unwrap_or(usize::MAX)
            .min(buffer.len());
        let read = reader
            .read(&mut buffer[..capacity])
            .map_err(|error| Failure::new("response_body", error.to_string()))?;
        if read == 0 {
            return Ok(body);
        }
        body.extend_from_slice(&buffer[..read]);
        remaining -= read as u64;
    }
}

fn copy_limited(
    reader: &mut impl Read,
    output: &mut impl Write,
    max_bytes: u64,
) -> Result<(), Failure> {
    let mut remaining = max_bytes;
    let mut buffer = [0_u8; 32 * 1024];

    loop {
        if remaining == 0 {
            let mut extra = [0_u8; 1];
            let read = reader
                .read(&mut extra)
                .map_err(|error| Failure::new("response_body", error.to_string()))?;
            if read == 0 {
                return Ok(());
            }
            return Err(Failure::response_too_large(max_bytes));
        }

        let capacity = usize::try_from(remaining)
            .unwrap_or(usize::MAX)
            .min(buffer.len());
        let read = reader
            .read(&mut buffer[..capacity])
            .map_err(|error| Failure::new("response_body", error.to_string()))?;
        if read == 0 {
            return Ok(());
        }
        output
            .write_all(&buffer[..read])
            .map_err(|error| Failure::new("output", error.to_string()))?;
        remaining -= read as u64;
    }
}

fn write_json(value: &impl Serialize) -> Result<(), String> {
    let stdout = io::stdout();
    let mut output = stdout.lock();
    serde_json::to_writer(&mut output, value).map_err(|error| error.to_string())?;
    output.write_all(b"\n").map_err(|error| error.to_string())?;
    output.flush().map_err(|error| error.to_string())
}
