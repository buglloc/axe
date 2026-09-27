//! Minimal read-only Porto RPC client.
//!
//! Only the proto2 messages behind `propertyList`, `list`, and `get` are
//! declared. Field numbers follow Porto's `rpc.proto`; the decoder skips every
//! other response field. Each frame is a varint length followed by the message.

use std::fmt;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use prost::Message;

pub(super) const MAX_MESSAGE_BYTES: usize = 16 << 20;

pub(super) const SUCCESS: i32 = 0;
pub(super) const UNKNOWN: i32 = 1;
pub(super) const CONTAINER_DOES_NOT_EXIST: i32 = 4;
const LOST_ERROR: i32 = 23;

#[derive(Clone, PartialEq, Message)]
struct Request {
    #[prost(message, optional, tag = "3")]
    list: Option<ListRequest>,
    #[prost(message, optional, tag = "11")]
    property_list: Option<PropertyListRequest>,
    #[prost(message, optional, tag = "15")]
    get: Option<GetRequest>,
}

#[derive(Clone, PartialEq, Message)]
struct ListRequest {}

#[derive(Clone, PartialEq, Message)]
struct PropertyListRequest {}

#[derive(Clone, PartialEq, Message)]
struct GetRequest {
    #[prost(string, repeated, tag = "1")]
    name: Vec<String>,
    #[prost(string, repeated, tag = "2")]
    variable: Vec<String>,
    #[prost(bool, optional, tag = "3")]
    nonblock: Option<bool>,
}

#[derive(Clone, PartialEq, Message)]
struct Response {
    #[prost(int32, optional, tag = "1")]
    error: Option<i32>,
    #[prost(string, optional, tag = "2")]
    error_msg: Option<String>,
    #[prost(message, optional, tag = "3")]
    list: Option<ListResponse>,
    #[prost(message, optional, tag = "6")]
    property_list: Option<PropertyListResponse>,
    #[prost(message, optional, tag = "10")]
    get: Option<GetResponse>,
}

#[derive(Clone, PartialEq, Message)]
struct ListResponse {
    #[prost(string, repeated, tag = "1")]
    name: Vec<String>,
}

#[derive(Clone, PartialEq, Message)]
struct PropertyListResponse {
    #[prost(message, repeated, tag = "1")]
    list: Vec<PropertyEntry>,
}

#[derive(Clone, PartialEq, Message)]
pub(super) struct PropertyEntry {
    #[prost(string, required, tag = "1")]
    pub name: String,
    #[prost(string, required, tag = "2")]
    pub desc: String,
    #[prost(bool, optional, tag = "3")]
    pub read_only: Option<bool>,
    #[prost(bool, optional, tag = "4")]
    pub dynamic: Option<bool>,
}

#[derive(Clone, PartialEq, Message)]
struct GetResponse {
    #[prost(message, repeated, tag = "1")]
    list: Vec<ContainerSnapshot>,
}

#[derive(Clone, PartialEq, Message)]
pub(super) struct ContainerSnapshot {
    #[prost(string, required, tag = "1")]
    pub name: String,
    #[prost(message, repeated, tag = "2")]
    pub keyval: Vec<PropertyValue>,
    #[prost(uint64, optional, tag = "3")]
    pub change_time: Option<u64>,
    #[prost(bool, optional, tag = "4")]
    pub no_changes: Option<bool>,
}

#[derive(Clone, PartialEq, Message)]
pub(super) struct PropertyValue {
    #[prost(string, required, tag = "1")]
    pub variable: String,
    #[prost(int32, optional, tag = "2")]
    pub error: Option<i32>,
    #[prost(string, optional, tag = "3")]
    pub error_msg: Option<String>,
    #[prost(string, optional, tag = "4")]
    pub value: Option<String>,
}

/// Returns the `EError` name Porto assigns to a numeric error code.
pub(super) fn error_name(code: i32) -> Option<&'static str> {
    Some(match code {
        0 => "Success",
        1 => "Unknown",
        2 => "InvalidMethod",
        3 => "ContainerAlreadyExists",
        4 => "ContainerDoesNotExist",
        5 => "InvalidProperty",
        6 => "InvalidData",
        7 => "InvalidValue",
        8 => "InvalidState",
        9 => "NotSupported",
        10 => "ResourceNotAvailable",
        11 => "Permission",
        12 => "VolumeAlreadyExists",
        13 => "VolumeNotFound",
        14 => "NoSpace",
        15 => "Busy",
        16 => "VolumeAlreadyLinked",
        17 => "VolumeNotLinked",
        18 => "LayerAlreadyExists",
        19 => "LayerNotFound",
        20 => "NoValue",
        21 => "VolumeNotReady",
        22 => "InvalidCommand",
        23 => "LostError",
        24 => "DeviceNotFound",
        25 => "InvalidPath",
        26 => "InvalidNetworkAddress",
        27 => "PortoFrozen",
        28 => "LabelNotFound",
        29 => "InvalidLabel",
        30 => "HelperError",
        31 => "HelperFatalError",
        32 => "InvalidFilesystem",
        33 => "NbdSocketTimeout",
        34 => "NbdSocketUnavaliable",
        35 => "NbdSocketError",
        36 => "NbdUnkownExport",
        37 => "NbdProtoError",
        404 => "NotFound",
        502 => "SocketError",
        503 => "SocketUnavailable",
        504 => "SocketTimeout",
        505 => "PortodReloaded",
        666 => "Taint",
        700 => "Docker",
        701 => "DockerImageNotFound",
        1000 => "Queued",
        _ => return None,
    })
}

#[derive(Debug)]
pub(super) struct ResponseError {
    pub code: i32,
    pub message: String,
}

impl fmt::Display for ResponseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match error_name(self.code) {
            Some(name) => write!(
                formatter,
                "Porto error {} ({name}): {}",
                self.code, self.message
            ),
            None => write!(
                formatter,
                "Porto error {} (unknown code): {}",
                self.code, self.message
            ),
        }
    }
}

#[derive(Debug)]
pub(super) enum Error {
    Io {
        operation: &'static str,
        source: io::Error,
    },
    Decode(prost::DecodeError),
    InvalidFrame(&'static str),
    MessageTooLarge {
        direction: &'static str,
        size: usize,
    },
    MissingResponse(&'static str),
    Response(ResponseError),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { operation, source } => write!(formatter, "{operation}: {source}"),
            Self::Decode(error) => write!(formatter, "decode Porto response: {error}"),
            Self::InvalidFrame(message) => write!(formatter, "invalid Porto frame: {message}"),
            Self::MessageTooLarge { direction, size } => write!(
                formatter,
                "Porto {direction} is {size} bytes; maximum is {MAX_MESSAGE_BYTES}"
            ),
            Self::MissingResponse(field) => {
                write!(formatter, "Porto response has no {field} payload")
            }
            Self::Response(error) => error.fmt(formatter),
        }
    }
}

pub(super) struct Client {
    stream: UnixStream,
}

impl Client {
    pub fn connect(socket: &Path) -> Result<Self, Error> {
        UnixStream::connect(socket)
            .map(|stream| Self { stream })
            .map_err(|source| io_error("connect to Porto socket", source))
    }

    pub fn list_properties(&mut self, timeout: Duration) -> Result<Vec<PropertyEntry>, Error> {
        let request = Request {
            property_list: Some(PropertyListRequest {}),
            ..Request::default()
        };
        let response = self.call(&request, timeout)?;
        response
            .property_list
            .map(|response| response.list)
            .ok_or(Error::MissingResponse("property_list"))
    }

    pub fn list(&mut self, timeout: Duration) -> Result<Vec<String>, Error> {
        let request = Request {
            list: Some(ListRequest {}),
            ..Request::default()
        };
        let response = self.call(&request, timeout)?;
        response
            .list
            .map(|response| response.name)
            .ok_or(Error::MissingResponse("list"))
    }

    /// Reads `variable` for every container in `name` without waiting on busy containers.
    pub fn get(
        &mut self,
        name: Vec<String>,
        variable: Vec<String>,
        timeout: Duration,
    ) -> Result<Vec<ContainerSnapshot>, Error> {
        let request = Request {
            get: Some(GetRequest {
                name,
                variable,
                nonblock: Some(true),
            }),
            ..Request::default()
        };
        let response = self.call(&request, timeout)?;
        response
            .get
            .map(|response| response.list)
            .ok_or(Error::MissingResponse("get"))
    }

    fn call(&mut self, request: &Request, timeout: Duration) -> Result<Response, Error> {
        self.stream
            .set_read_timeout(Some(timeout))
            .map_err(|source| io_error("set Porto read timeout", source))?;
        self.stream
            .set_write_timeout(Some(timeout))
            .map_err(|source| io_error("set Porto write timeout", source))?;

        let size = request.encoded_len();
        if size > MAX_MESSAGE_BYTES {
            return Err(Error::MessageTooLarge {
                direction: "request",
                size,
            });
        }
        self.stream
            .write_all(&request.encode_length_delimited_to_vec())
            .map_err(|source| io_error("write Porto request", source))?;

        let size = read_frame_length(&mut self.stream)?;
        if size > MAX_MESSAGE_BYTES {
            return Err(Error::MessageTooLarge {
                direction: "response",
                size,
            });
        }
        let mut payload = vec![0_u8; size];
        self.stream
            .read_exact(&mut payload)
            .map_err(|source| io_error("read Porto response", source))?;
        let response = Response::decode(payload.as_slice()).map_err(Error::Decode)?;

        let code = response.error.unwrap_or(LOST_ERROR);
        if code != SUCCESS {
            return Err(Error::Response(ResponseError {
                code,
                message: response.error_msg.unwrap_or_default(),
            }));
        }

        Ok(response)
    }
}

fn io_error(operation: &'static str, source: io::Error) -> Error {
    Error::Io { operation, source }
}

fn read_frame_length(reader: &mut impl Read) -> Result<usize, Error> {
    let mut value = 0_u32;

    for shift in (0..=28).step_by(7) {
        let mut byte = [0_u8; 1];
        reader
            .read_exact(&mut byte)
            .map_err(|source| io_error("read Porto response length", source))?;
        let byte = byte[0];

        if shift == 28 && byte & 0xf0 != 0 {
            return Err(Error::InvalidFrame("length varint exceeds u32"));
        }

        value |= u32::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value as usize);
        }
    }

    Err(Error::InvalidFrame("unterminated length varint"))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;

    use super::*;

    const TIMEOUT: Duration = Duration::from_secs(5);

    static NEXT_SOCKET: AtomicU64 = AtomicU64::new(0);

    fn socket_path() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "vzik-porto-{}-{}",
            std::process::id(),
            NEXT_SOCKET.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_file(&path);
        path
    }

    fn serve_once(path: &Path, frame: Vec<u8>) -> thread::JoinHandle<Request> {
        let listener = UnixListener::bind(path).expect("bind fake Porto socket");
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept Porto client");
            let length = read_frame_length(&mut stream).expect("read request length");
            let mut payload = vec![0; length];
            stream.read_exact(&mut payload).expect("read request");
            stream.write_all(&frame).expect("write response");
            Request::decode(payload.as_slice()).expect("decode request")
        })
    }

    #[test]
    fn get_round_trip_uses_porto_framing() {
        let path = socket_path();
        let response = Response {
            error: Some(SUCCESS),
            get: Some(GetResponse {
                list: vec![ContainerSnapshot {
                    name: "a".into(),
                    keyval: vec![PropertyValue {
                        variable: "state".into(),
                        value: Some("running".into()),
                        ..PropertyValue::default()
                    }],
                    ..ContainerSnapshot::default()
                }],
            }),
            ..Response::default()
        };
        let server = serve_once(&path, response.encode_length_delimited_to_vec());
        let mut client = Client::connect(&path).expect("connect");

        let snapshots = client
            .get(vec!["a".into()], vec!["state".into()], TIMEOUT)
            .expect("get");

        assert_eq!(snapshots[0].keyval[0].value.as_deref(), Some("running"));
        let request = server.join().expect("join server");
        let get = request.get.expect("get request");
        assert_eq!(get.name, ["a"]);
        assert_eq!(get.variable, ["state"]);
        assert_eq!(get.nonblock, Some(true));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn response_errors_preserve_known_porto_code() {
        let path = socket_path();
        let response = Response {
            error: Some(11),
            error_msg: Some("permission denied".into()),
            ..Response::default()
        };
        let server = serve_once(&path, response.encode_length_delimited_to_vec());
        let mut client = Client::connect(&path).expect("connect");

        let error = client.list(TIMEOUT).expect_err("Porto error must fail");
        let Error::Response(error) = error else {
            panic!("unexpected error: {error}");
        };

        assert_eq!(error.code, 11);
        assert_eq!(
            error.to_string(),
            "Porto error 11 (Permission): permission denied"
        );
        assert!(server.join().expect("join server").list.is_some());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn missing_error_code_is_reported_as_lost_error() {
        let path = socket_path();
        let server = serve_once(&path, Response::default().encode_length_delimited_to_vec());
        let mut client = Client::connect(&path).expect("connect");

        let error = client
            .list_properties(TIMEOUT)
            .expect_err("missing error code must fail");

        assert!(matches!(
            error,
            Error::Response(ResponseError {
                code: LOST_ERROR,
                ..
            })
        ));
        assert!(server.join().expect("join server").property_list.is_some());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn oversized_response_is_rejected_before_allocation() {
        let path = socket_path();
        let mut frame = Vec::new();
        prost::encoding::encode_varint(MAX_MESSAGE_BYTES as u64 + 1, &mut frame);
        let server = serve_once(&path, frame);
        let mut client = Client::connect(&path).expect("connect");

        let error = client
            .list(TIMEOUT)
            .expect_err("oversized response must fail");

        assert!(matches!(
            error,
            Error::MessageTooLarge {
                direction: "response",
                size,
            } if size == MAX_MESSAGE_BYTES + 1
        ));
        server.join().expect("join server");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn frame_length_rejects_values_beyond_u32() {
        let error = read_frame_length(&mut [0xff, 0xff, 0xff, 0xff, 0x1f].as_slice())
            .expect_err("oversized varint must fail");

        assert!(matches!(
            error,
            Error::InvalidFrame("length varint exceeds u32")
        ));
    }
}
