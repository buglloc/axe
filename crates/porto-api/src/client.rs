use std::fmt;
use std::io;
#[cfg(unix)]
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use prost::Message;

use crate::rpc;

pub const DEFAULT_SOCKET_PATH: &str = "/run/portod.socket";
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);
pub const DEFAULT_MAX_MESSAGE_BYTES: usize = 32 << 20;

#[derive(Debug)]
pub struct ResponseError {
    pub code: i32,
    pub kind: Option<rpc::EError>,
    pub message: String,
}

impl fmt::Display for ResponseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            Some(kind) => write!(
                formatter,
                "Porto error {} ({}): {}",
                self.code,
                kind.as_str_name(),
                self.message
            ),
            None => write!(
                formatter,
                "Porto error {} (unknown code): {}",
                self.code, self.message
            ),
        }
    }
}

impl std::error::Error for ResponseError {}

#[derive(Debug)]
pub enum Error {
    Io {
        operation: &'static str,
        source: io::Error,
    },
    Encode(prost::EncodeError),
    Decode(prost::DecodeError),
    InvalidFrame(&'static str),
    MessageTooLarge {
        direction: &'static str,
        size: usize,
        maximum: usize,
    },
    MissingResponse(&'static str),
    InvalidConfiguration(&'static str),
    Response(ResponseError),
    Unsupported,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { operation, source } => write!(formatter, "{operation}: {source}"),
            Self::Encode(error) => write!(formatter, "encode Porto request: {error}"),
            Self::Decode(error) => write!(formatter, "decode Porto response: {error}"),
            Self::InvalidFrame(message) => write!(formatter, "invalid Porto frame: {message}"),
            Self::MessageTooLarge {
                direction,
                size,
                maximum,
            } => write!(
                formatter,
                "Porto {direction} is {size} bytes; maximum is {maximum}"
            ),
            Self::MissingResponse(field) => {
                write!(formatter, "Porto response has no {field} payload")
            }
            Self::InvalidConfiguration(message) => {
                write!(formatter, "invalid Porto client configuration: {message}")
            }
            Self::Response(error) => error.fmt(formatter),
            Self::Unsupported => formatter.write_str("Porto Unix socket transport is unsupported"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Encode(error) => Some(error),
            Self::Decode(error) => Some(error),
            Self::Response(error) => Some(error),
            _ => None,
        }
    }
}

impl From<prost::EncodeError> for Error {
    fn from(error: prost::EncodeError) -> Self {
        Self::Encode(error)
    }
}

impl From<prost::DecodeError> for Error {
    fn from(error: prost::DecodeError) -> Self {
        Self::Decode(error)
    }
}

#[derive(Debug)]
pub struct Client {
    socket_path: PathBuf,
    timeout: Option<Duration>,
    max_message_bytes: usize,
    #[cfg(unix)]
    stream: Option<std::os::unix::net::UnixStream>,
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

impl Client {
    #[must_use]
    pub fn new() -> Self {
        Self::with_socket_path(DEFAULT_SOCKET_PATH)
    }

    #[must_use]
    pub fn with_socket_path(path: impl Into<PathBuf>) -> Self {
        Self {
            socket_path: path.into(),
            timeout: Some(DEFAULT_TIMEOUT),
            max_message_bytes: DEFAULT_MAX_MESSAGE_BYTES,
            #[cfg(unix)]
            stream: None,
        }
    }

    #[must_use]
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    #[must_use]
    pub fn timeout(&self) -> Option<Duration> {
        self.timeout
    }

    #[must_use]
    pub fn max_message_bytes(&self) -> usize {
        self.max_message_bytes
    }

    pub fn set_timeout(&mut self, timeout: Option<Duration>) -> Result<(), Error> {
        if timeout == Some(Duration::ZERO) {
            return Err(Error::InvalidConfiguration("timeout must be positive"));
        }

        self.timeout = timeout;
        #[cfg(unix)]
        if let Some(stream) = &self.stream {
            configure_stream(stream, timeout)?;
        }

        Ok(())
    }

    pub fn set_max_message_bytes(&mut self, maximum: usize) -> Result<(), Error> {
        if maximum == 0 || maximum > u32::MAX as usize {
            return Err(Error::InvalidConfiguration(
                "message limit must be between 1 and u32::MAX",
            ));
        }

        self.max_message_bytes = maximum;
        Ok(())
    }

    pub fn is_connected(&self) -> bool {
        #[cfg(unix)]
        {
            self.stream.is_some()
        }
        #[cfg(not(unix))]
        {
            false
        }
    }

    pub fn connect(&mut self) -> Result<(), Error> {
        #[cfg(unix)]
        {
            if self.stream.is_some() {
                return Ok(());
            }

            let stream = std::os::unix::net::UnixStream::connect(&self.socket_path)
                .map_err(|source| io_error("connect to Porto socket", source))?;
            configure_stream(&stream, self.timeout)?;
            self.stream = Some(stream);
            Ok(())
        }
        #[cfg(not(unix))]
        {
            Err(Error::Unsupported)
        }
    }

    pub fn close(&mut self) {
        #[cfg(unix)]
        {
            self.stream = None;
        }
    }

    pub fn write_request(&mut self, request: &rpc::TContainerRequest) -> Result<(), Error> {
        let encoded_len = request.encoded_len();
        self.check_message_size("request", encoded_len)?;

        let mut frame = Vec::with_capacity(encoded_len.saturating_add(5));
        request.encode_length_delimited(&mut frame)?;

        self.connect()?;
        #[cfg(unix)]
        if let Err(source) = self
            .stream
            .as_mut()
            .expect("connected Porto stream")
            .write_all(&frame)
        {
            self.close();
            return Err(io_error("write Porto request", source));
        }

        Ok(())
    }

    pub fn read_response(&mut self) -> Result<rpc::TContainerResponse, Error> {
        self.connect()?;

        #[cfg(unix)]
        let result = (|| {
            let maximum = self.max_message_bytes;
            let stream = self.stream.as_mut().expect("connected Porto stream");
            let length = read_frame_length(stream)?;
            if length > maximum {
                return Err(Error::MessageTooLarge {
                    direction: "response",
                    size: length,
                    maximum,
                });
            }

            let mut payload = vec![0_u8; length];
            stream
                .read_exact(&mut payload)
                .map_err(|source| io_error("read Porto response", source))?;

            rpc::TContainerResponse::decode(payload.as_slice()).map_err(Error::from)
        })();
        #[cfg(not(unix))]
        let result: Result<rpc::TContainerResponse, Error> = Err(Error::Unsupported);

        let response = match result {
            Ok(response) => response,
            Err(error) => {
                self.close();
                return Err(error);
            }
        };

        let code = response.error.unwrap_or(rpc::EError::LostError as i32);
        if code != rpc::EError::Success as i32 {
            return Err(Error::Response(ResponseError {
                code,
                kind: rpc::EError::try_from(code).ok(),
                message: response.error_msg.clone().unwrap_or_default(),
            }));
        }

        Ok(response)
    }

    pub fn call(
        &mut self,
        request: &rpc::TContainerRequest,
    ) -> Result<rpc::TContainerResponse, Error> {
        self.write_request(request)?;
        self.read_response()
    }

    pub fn call_with_timeout(
        &mut self,
        request: &rpc::TContainerRequest,
        timeout: Duration,
    ) -> Result<rpc::TContainerResponse, Error> {
        let previous = self.timeout;
        self.set_timeout(Some(timeout))?;
        let result = self.call(request);
        let restore = self.set_timeout(previous);

        match (result, restore) {
            (Ok(response), Ok(())) => Ok(response),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
        }
    }

    fn check_message_size(&self, direction: &'static str, size: usize) -> Result<(), Error> {
        if size > self.max_message_bytes {
            return Err(Error::MessageTooLarge {
                direction,
                size,
                maximum: self.max_message_bytes,
            });
        }

        Ok(())
    }

    pub fn version(&mut self) -> Result<rpc::TVersionResponse, Error> {
        response_method(
            self,
            rpc::TContainerRequest {
                version: Some(rpc::TVersionRequest {}),
                ..Default::default()
            },
            |response| response.version,
            "version",
        )
    }

    pub fn create(&mut self, request: rpc::TContainerCreateRequest) -> Result<(), Error> {
        unit_method(
            self,
            rpc::TContainerRequest {
                create: Some(request),
                ..Default::default()
            },
        )
    }

    pub fn create_weak(&mut self, request: rpc::TContainerCreateRequest) -> Result<(), Error> {
        unit_method(
            self,
            rpc::TContainerRequest {
                create_weak: Some(request),
                ..Default::default()
            },
        )
    }

    pub fn destroy(&mut self, request: rpc::TContainerDestroyRequest) -> Result<(), Error> {
        unit_method(
            self,
            rpc::TContainerRequest {
                destroy: Some(request),
                ..Default::default()
            },
        )
    }

    pub fn start(&mut self, request: rpc::TContainerStartRequest) -> Result<(), Error> {
        unit_method(
            self,
            rpc::TContainerRequest {
                start: Some(request),
                ..Default::default()
            },
        )
    }

    pub fn stop(&mut self, request: rpc::TContainerStopRequest) -> Result<(), Error> {
        unit_method(
            self,
            rpc::TContainerRequest {
                stop: Some(request),
                ..Default::default()
            },
        )
    }

    pub fn kill(&mut self, request: rpc::TContainerKillRequest) -> Result<(), Error> {
        unit_method(
            self,
            rpc::TContainerRequest {
                kill: Some(request),
                ..Default::default()
            },
        )
    }

    pub fn pause(&mut self, request: rpc::TContainerPauseRequest) -> Result<(), Error> {
        unit_method(
            self,
            rpc::TContainerRequest {
                pause: Some(request),
                ..Default::default()
            },
        )
    }

    pub fn resume(&mut self, request: rpc::TContainerResumeRequest) -> Result<(), Error> {
        unit_method(
            self,
            rpc::TContainerRequest {
                resume: Some(request),
                ..Default::default()
            },
        )
    }

    pub fn respawn(&mut self, request: rpc::TContainerRespawnRequest) -> Result<(), Error> {
        unit_method(
            self,
            rpc::TContainerRequest {
                respawn: Some(request),
                ..Default::default()
            },
        )
    }

    pub fn wait(
        &mut self,
        request: rpc::TContainerWaitRequest,
    ) -> Result<rpc::TContainerWaitResponse, Error> {
        response_method(
            self,
            rpc::TContainerRequest {
                wait: Some(request),
                ..Default::default()
            },
            |response| response.wait,
            "wait",
        )
    }

    pub fn async_wait(
        &mut self,
        request: rpc::TContainerWaitRequest,
    ) -> Result<rpc::TContainerWaitResponse, Error> {
        response_method(
            self,
            rpc::TContainerRequest {
                async_wait: Some(request),
                ..Default::default()
            },
            |response| response.async_wait,
            "async_wait",
        )
    }

    pub fn stop_async_wait(&mut self, request: rpc::TContainerWaitRequest) -> Result<(), Error> {
        unit_method(
            self,
            rpc::TContainerRequest {
                stop_async_wait: Some(request),
                ..Default::default()
            },
        )
    }

    pub fn list(
        &mut self,
        request: rpc::TContainerListRequest,
    ) -> Result<rpc::TContainerListResponse, Error> {
        response_method(
            self,
            rpc::TContainerRequest {
                list: Some(request),
                ..Default::default()
            },
            |response| response.list,
            "list",
        )
    }

    pub fn list_properties(&mut self) -> Result<rpc::TContainerPropertyListResponse, Error> {
        response_method(
            self,
            rpc::TContainerRequest {
                property_list: Some(rpc::TContainerPropertyListRequest {}),
                ..Default::default()
            },
            |response| response.property_list,
            "property_list",
        )
    }

    #[deprecated(note = "Porto data list is deprecated; use list_properties")]
    pub fn list_data(&mut self) -> Result<rpc::TContainerDataListResponse, Error> {
        response_method(
            self,
            rpc::TContainerRequest {
                data_list: Some(rpc::TContainerDataListRequest {}),
                ..Default::default()
            },
            |response| response.data_list,
            "data_list",
        )
    }

    pub fn get(
        &mut self,
        request: rpc::TContainerGetRequest,
    ) -> Result<rpc::TContainerGetResponse, Error> {
        response_method(
            self,
            rpc::TContainerRequest {
                get: Some(request),
                ..Default::default()
            },
            |response| response.get,
            "get",
        )
    }

    pub fn get_property(
        &mut self,
        request: rpc::TContainerGetPropertyRequest,
    ) -> Result<rpc::TContainerGetPropertyResponse, Error> {
        response_method(
            self,
            rpc::TContainerRequest {
                get_property: Some(request),
                ..Default::default()
            },
            |response| response.get_property,
            "get_property",
        )
    }

    pub fn set_property(
        &mut self,
        request: rpc::TContainerSetPropertyRequest,
    ) -> Result<(), Error> {
        unit_method(
            self,
            rpc::TContainerRequest {
                set_property: Some(request),
                ..Default::default()
            },
        )
    }

    #[deprecated(note = "Porto data access is deprecated; use get_property")]
    pub fn get_data(
        &mut self,
        request: rpc::TContainerGetDataRequest,
    ) -> Result<rpc::TContainerGetDataResponse, Error> {
        response_method(
            self,
            rpc::TContainerRequest {
                get_data: Some(request),
                ..Default::default()
            },
            |response| response.get_data,
            "get_data",
        )
    }

    pub fn find_label(
        &mut self,
        request: rpc::TFindLabelRequest,
    ) -> Result<rpc::TFindLabelResponse, Error> {
        response_method(
            self,
            rpc::TContainerRequest {
                find_label: Some(request),
                ..Default::default()
            },
            |response| response.find_label,
            "find_label",
        )
    }

    pub fn set_label(
        &mut self,
        request: rpc::TSetLabelRequest,
    ) -> Result<rpc::TSetLabelResponse, Error> {
        response_method(
            self,
            rpc::TContainerRequest {
                set_label: Some(request),
                ..Default::default()
            },
            |response| response.set_label,
            "set_label",
        )
    }

    pub fn inc_label(
        &mut self,
        request: rpc::TIncLabelRequest,
    ) -> Result<rpc::TIncLabelResponse, Error> {
        response_method(
            self,
            rpc::TContainerRequest {
                inc_label: Some(request),
                ..Default::default()
            },
            |response| response.inc_label,
            "inc_label",
        )
    }

    pub fn create_from_spec(&mut self, request: rpc::TCreateFromSpecRequest) -> Result<(), Error> {
        unit_method(
            self,
            rpc::TContainerRequest {
                create_from_spec: Some(request),
                ..Default::default()
            },
        )
    }

    pub fn update_from_spec(&mut self, request: rpc::TUpdateFromSpecRequest) -> Result<(), Error> {
        unit_method(
            self,
            rpc::TContainerRequest {
                update_from_spec: Some(request),
                ..Default::default()
            },
        )
    }

    pub fn list_containers_by(
        &mut self,
        request: rpc::TListContainersRequest,
    ) -> Result<rpc::TListContainersResponse, Error> {
        response_method(
            self,
            rpc::TContainerRequest {
                list_containers_by: Some(request),
                ..Default::default()
            },
            |response| response.list_containers_by,
            "list_containers_by",
        )
    }

    pub fn list_volume_properties(&mut self) -> Result<rpc::TVolumePropertyListResponse, Error> {
        response_method(
            self,
            rpc::TContainerRequest {
                list_volume_properties: Some(rpc::TVolumePropertyListRequest {}),
                ..Default::default()
            },
            |response| response.volume_property_list,
            "volume_property_list",
        )
    }

    pub fn create_volume(
        &mut self,
        request: rpc::TVolumeCreateRequest,
    ) -> Result<rpc::TVolumeDescription, Error> {
        response_method(
            self,
            rpc::TContainerRequest {
                create_volume: Some(request),
                ..Default::default()
            },
            |response| response.volume_description,
            "volume_description",
        )
    }

    pub fn link_volume(&mut self, request: rpc::TVolumeLinkRequest) -> Result<(), Error> {
        unit_method(
            self,
            rpc::TContainerRequest {
                link_volume: Some(request),
                ..Default::default()
            },
        )
    }

    pub fn link_volume_target(&mut self, request: rpc::TVolumeLinkRequest) -> Result<(), Error> {
        unit_method(
            self,
            rpc::TContainerRequest {
                link_volume_target: Some(request),
                ..Default::default()
            },
        )
    }

    pub fn unlink_volume(&mut self, request: rpc::TVolumeUnlinkRequest) -> Result<(), Error> {
        unit_method(
            self,
            rpc::TContainerRequest {
                unlink_volume: Some(request),
                ..Default::default()
            },
        )
    }

    pub fn unlink_volume_target(
        &mut self,
        request: rpc::TVolumeUnlinkRequest,
    ) -> Result<(), Error> {
        unit_method(
            self,
            rpc::TContainerRequest {
                unlink_volume_target: Some(request),
                ..Default::default()
            },
        )
    }

    pub fn list_volumes(
        &mut self,
        request: rpc::TVolumeListRequest,
    ) -> Result<rpc::TVolumeListResponse, Error> {
        response_method(
            self,
            rpc::TContainerRequest {
                list_volumes: Some(request),
                ..Default::default()
            },
            |response| response.volume_list,
            "volume_list",
        )
    }

    pub fn tune_volume(&mut self, request: rpc::TVolumeTuneRequest) -> Result<(), Error> {
        unit_method(
            self,
            rpc::TContainerRequest {
                tune_volume: Some(request),
                ..Default::default()
            },
        )
    }

    pub fn check_volume(
        &mut self,
        request: rpc::TVolumeCheckRequest,
    ) -> Result<rpc::TVolumeCheckResponse, Error> {
        response_method(
            self,
            rpc::TContainerRequest {
                check_volume: Some(request),
                ..Default::default()
            },
            |response| response.check_volume,
            "check_volume",
        )
    }

    pub fn new_volume(
        &mut self,
        request: rpc::TNewVolumeRequest,
    ) -> Result<rpc::TNewVolumeResponse, Error> {
        response_method(
            self,
            rpc::TContainerRequest {
                new_volume: Some(request),
                ..Default::default()
            },
            |response| response.new_volume,
            "new_volume",
        )
    }

    pub fn get_volume(
        &mut self,
        request: rpc::TGetVolumeRequest,
    ) -> Result<rpc::TGetVolumeResponse, Error> {
        response_method(
            self,
            rpc::TContainerRequest {
                get_volume: Some(request),
                ..Default::default()
            },
            |response| response.get_volume,
            "get_volume",
        )
    }

    pub fn import_layer(&mut self, request: rpc::TLayerImportRequest) -> Result<(), Error> {
        unit_method(
            self,
            rpc::TContainerRequest {
                import_layer: Some(request),
                ..Default::default()
            },
        )
    }

    pub fn export_layer(&mut self, request: rpc::TLayerExportRequest) -> Result<(), Error> {
        unit_method(
            self,
            rpc::TContainerRequest {
                export_layer: Some(request),
                ..Default::default()
            },
        )
    }

    pub fn remove_layer(&mut self, request: rpc::TLayerRemoveRequest) -> Result<(), Error> {
        unit_method(
            self,
            rpc::TContainerRequest {
                remove_layer: Some(request),
                ..Default::default()
            },
        )
    }

    pub fn list_layers(
        &mut self,
        request: rpc::TLayerListRequest,
    ) -> Result<rpc::TLayerListResponse, Error> {
        response_method(
            self,
            rpc::TContainerRequest {
                list_layers: Some(request),
                ..Default::default()
            },
            |response| response.layers,
            "layers",
        )
    }

    pub fn get_layer_private(
        &mut self,
        request: rpc::TLayerGetPrivateRequest,
    ) -> Result<rpc::TLayerGetPrivateResponse, Error> {
        response_method(
            self,
            rpc::TContainerRequest {
                getlayerprivate: Some(request),
                ..Default::default()
            },
            |response| response.layer_private,
            "layer_private",
        )
    }

    pub fn set_layer_private(
        &mut self,
        request: rpc::TLayerSetPrivateRequest,
    ) -> Result<(), Error> {
        unit_method(
            self,
            rpc::TContainerRequest {
                setlayerprivate: Some(request),
                ..Default::default()
            },
        )
    }

    pub fn list_storage(
        &mut self,
        request: rpc::TStorageListRequest,
    ) -> Result<rpc::TStorageListResponse, Error> {
        response_method(
            self,
            rpc::TContainerRequest {
                list_storage: Some(request),
                ..Default::default()
            },
            |response| response.storage_list,
            "storage_list",
        )
    }

    pub fn remove_storage(&mut self, request: rpc::TStorageRemoveRequest) -> Result<(), Error> {
        unit_method(
            self,
            rpc::TContainerRequest {
                remove_storage: Some(request),
                ..Default::default()
            },
        )
    }

    pub fn import_storage(&mut self, request: rpc::TStorageImportRequest) -> Result<(), Error> {
        unit_method(
            self,
            rpc::TContainerRequest {
                import_storage: Some(request),
                ..Default::default()
            },
        )
    }

    pub fn export_storage(&mut self, request: rpc::TStorageExportRequest) -> Result<(), Error> {
        unit_method(
            self,
            rpc::TContainerRequest {
                export_storage: Some(request),
                ..Default::default()
            },
        )
    }

    pub fn cleanup_place(&mut self, request: rpc::TCleanupPlaceRequest) -> Result<(), Error> {
        unit_method(
            self,
            rpc::TContainerRequest {
                cleanup_place: Some(request),
                ..Default::default()
            },
        )
    }

    pub fn create_meta_storage(&mut self, request: rpc::TMetaStorage) -> Result<(), Error> {
        unit_method(
            self,
            rpc::TContainerRequest {
                create_meta_storage: Some(request),
                ..Default::default()
            },
        )
    }

    pub fn resize_meta_storage(&mut self, request: rpc::TMetaStorage) -> Result<(), Error> {
        unit_method(
            self,
            rpc::TContainerRequest {
                resize_meta_storage: Some(request),
                ..Default::default()
            },
        )
    }

    pub fn remove_meta_storage(&mut self, request: rpc::TMetaStorage) -> Result<(), Error> {
        unit_method(
            self,
            rpc::TContainerRequest {
                remove_meta_storage: Some(request),
                ..Default::default()
            },
        )
    }

    pub fn set_symlink(&mut self, request: rpc::TSetSymlinkRequest) -> Result<(), Error> {
        unit_method(
            self,
            rpc::TContainerRequest {
                set_symlink: Some(request),
                ..Default::default()
            },
        )
    }

    pub fn convert_path(
        &mut self,
        request: rpc::TConvertPathRequest,
    ) -> Result<rpc::TConvertPathResponse, Error> {
        response_method(
            self,
            rpc::TContainerRequest {
                convert_path: Some(request),
                ..Default::default()
            },
            |response| response.convert_path,
            "convert_path",
        )
    }

    pub fn attach_process(&mut self, request: rpc::TAttachProcessRequest) -> Result<(), Error> {
        unit_method(
            self,
            rpc::TContainerRequest {
                attach_process: Some(request),
                ..Default::default()
            },
        )
    }

    pub fn attach_thread(&mut self, request: rpc::TAttachProcessRequest) -> Result<(), Error> {
        unit_method(
            self,
            rpc::TContainerRequest {
                attach_thread: Some(request),
                ..Default::default()
            },
        )
    }

    pub fn locate_process(
        &mut self,
        request: rpc::TLocateProcessRequest,
    ) -> Result<rpc::TLocateProcessResponse, Error> {
        response_method(
            self,
            rpc::TContainerRequest {
                locate_process: Some(request),
                ..Default::default()
            },
            |response| response.locate_process,
            "locate_process",
        )
    }

    pub fn get_system(&mut self) -> Result<rpc::TGetSystemResponse, Error> {
        response_method(
            self,
            rpc::TContainerRequest {
                get_system: Some(rpc::TGetSystemRequest {}),
                ..Default::default()
            },
            |response| response.get_system,
            "get_system",
        )
    }

    pub fn set_system(
        &mut self,
        request: rpc::TSetSystemRequest,
    ) -> Result<rpc::TSetSystemResponse, Error> {
        response_method(
            self,
            rpc::TContainerRequest {
                set_system: Some(request),
                ..Default::default()
            },
            |response| response.set_system,
            "set_system",
        )
    }

    pub fn clear_statistics(&mut self, request: rpc::TClearStatisticsRequest) -> Result<(), Error> {
        unit_method(
            self,
            rpc::TContainerRequest {
                clear_statistics: Some(request),
                ..Default::default()
            },
        )
    }

    pub fn docker_image_status(
        &mut self,
        request: rpc::TDockerImageStatusRequest,
    ) -> Result<rpc::TDockerImageStatusResponse, Error> {
        response_method(
            self,
            rpc::TContainerRequest {
                docker_image_status: Some(request),
                ..Default::default()
            },
            |response| response.docker_image_status,
            "docker_image_status",
        )
    }

    pub fn list_docker_images(
        &mut self,
        request: rpc::TDockerImageListRequest,
    ) -> Result<rpc::TDockerImageListResponse, Error> {
        response_method(
            self,
            rpc::TContainerRequest {
                list_docker_images: Some(request),
                ..Default::default()
            },
            |response| response.list_docker_images,
            "list_docker_images",
        )
    }

    pub fn pull_docker_image(
        &mut self,
        request: rpc::TDockerImagePullRequest,
    ) -> Result<rpc::TDockerImagePullResponse, Error> {
        response_method(
            self,
            rpc::TContainerRequest {
                pull_docker_image: Some(request),
                ..Default::default()
            },
            |response| response.pull_docker_image,
            "pull_docker_image",
        )
    }

    pub fn remove_docker_image(
        &mut self,
        request: rpc::TDockerImageRemoveRequest,
    ) -> Result<(), Error> {
        unit_method(
            self,
            rpc::TContainerRequest {
                remove_docker_image: Some(request),
                ..Default::default()
            },
        )
    }
}

fn unit_method(client: &mut Client, request: rpc::TContainerRequest) -> Result<(), Error> {
    client.call(&request).map(|_| ())
}

fn response_method<T>(
    client: &mut Client,
    request: rpc::TContainerRequest,
    extract: impl FnOnce(rpc::TContainerResponse) -> Option<T>,
    field: &'static str,
) -> Result<T, Error> {
    let response = client.call(&request)?;
    extract(response).ok_or(Error::MissingResponse(field))
}

fn io_error(operation: &'static str, source: io::Error) -> Error {
    Error::Io { operation, source }
}

#[cfg(unix)]
fn configure_stream(
    stream: &std::os::unix::net::UnixStream,
    timeout: Option<Duration>,
) -> Result<(), Error> {
    stream
        .set_read_timeout(timeout)
        .map_err(|source| io_error("set Porto read timeout", source))?;
    stream
        .set_write_timeout(timeout)
        .map_err(|source| io_error("set Porto write timeout", source))?;
    Ok(())
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
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;

    use super::*;

    static NEXT_SOCKET: AtomicU64 = AtomicU64::new(0);

    fn socket_path(_name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "pa-{}-{}",
            std::process::id(),
            NEXT_SOCKET.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_file(&path);
        path
    }

    fn serve_once(
        path: PathBuf,
        response: rpc::TContainerResponse,
    ) -> thread::JoinHandle<rpc::TContainerRequest> {
        let listener = UnixListener::bind(&path).expect("bind fake Porto socket");
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept Porto client");
            let length = read_frame_length(&mut stream).expect("read request length");
            let mut payload = vec![0; length];
            stream.read_exact(&mut payload).expect("read request");
            let request =
                rpc::TContainerRequest::decode(payload.as_slice()).expect("decode request");
            let frame = response.encode_length_delimited_to_vec();
            stream.write_all(&frame).expect("write response");
            request
        })
    }

    #[test]
    fn version_round_trip_uses_porto_framing() {
        let path = socket_path("version");
        let server = serve_once(
            path.clone(),
            rpc::TContainerResponse {
                error: Some(rpc::EError::Success as i32),
                version: Some(rpc::TVersionResponse {
                    tag: "5.3.59".into(),
                    revision: "r123".into(),
                }),
                ..Default::default()
            },
        );
        let mut client = Client::with_socket_path(&path);

        let version = client.version().expect("read version");

        assert_eq!(version.tag, "5.3.59");
        assert!(server.join().expect("join server").version.is_some());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn response_errors_preserve_known_porto_code() {
        let path = socket_path("response-error");
        let server = serve_once(
            path.clone(),
            rpc::TContainerResponse {
                error: Some(rpc::EError::Permission as i32),
                error_msg: Some("permission denied".into()),
                ..Default::default()
            },
        );
        let mut client = Client::with_socket_path(&path);

        let error = client
            .list(rpc::TContainerListRequest::default())
            .expect_err("Porto error must fail the call");
        let Error::Response(error) = error else {
            panic!("unexpected error: {error}");
        };

        assert_eq!(error.code, rpc::EError::Permission as i32);
        assert_eq!(error.kind, Some(rpc::EError::Permission));
        assert_eq!(error.message, "permission denied");
        server.join().expect("join server");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn oversized_response_is_rejected_before_allocation() {
        let path = socket_path("oversized");
        let listener = UnixListener::bind(&path).expect("bind fake Porto socket");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept Porto client");
            let length = read_frame_length(&mut stream).expect("read request length");
            let mut payload = vec![0; length];
            stream.read_exact(&mut payload).expect("read request");
            stream.write_all(&[0x81, 0x08]).expect("write length");
        });
        let mut client = Client::with_socket_path(&path);
        client.set_max_message_bytes(1024).expect("set limit");

        let error = client.version().expect_err("oversized response must fail");

        assert!(matches!(
            error,
            Error::MessageTooLarge {
                direction: "response",
                size: 1025,
                maximum: 1024
            }
        ));
        server.join().expect("join server");
        let _ = fs::remove_file(path);
    }
}
