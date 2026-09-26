#![forbid(unsafe_code)]

//! Complete typed bindings and client transport for the Porto RPC API.

mod client;

pub use client::{
    Client, DEFAULT_MAX_MESSAGE_BYTES, DEFAULT_SOCKET_PATH, DEFAULT_TIMEOUT, Error, ResponseError,
};

pub mod rpc {
    include!(concat!(env!("OUT_DIR"), "/rpc.rs"));
}

pub mod seccomp {
    include!(concat!(env!("OUT_DIR"), "/seccomp.rs"));
}
