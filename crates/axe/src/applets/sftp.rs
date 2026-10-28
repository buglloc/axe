use std::collections::HashMap;
use std::io::{self, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use bytes::Bytes;
use russh_sftp::extensions::{EXPAND_PATH, ExpandPathExtension};
use russh_sftp::protocol::{
    Attrs, Data, File, FileAttributes, Handle, Name, OpenFlags, Packet, Status, StatusCode, Version,
};
use russh_sftp::server::{Handler, StatusReply};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWrite, AsyncWriteExt};

const DIRECTORY_BATCH_SIZE: usize = 64;
const MAX_CLIENT_PACKET_SIZE: u32 = 256 * 1024;
const MAX_TRANSFER_SIZE: usize = 240 * 1024;
const MAX_OPEN_HANDLES: usize = 256;

enum OpenHandle {
    File {
        file: tokio::fs::File,
        path: PathBuf,
    },
    Directory {
        entries: tokio::fs::ReadDir,
        path: PathBuf,
    },
}

pub struct Filesystem {
    workdir: PathBuf,
    home_dir: PathBuf,
    next_handle: u64,
    handles: HashMap<String, OpenHandle>,
}

impl Filesystem {
    pub fn with_home(workdir: PathBuf, home_dir: PathBuf) -> Self {
        Self {
            workdir,
            home_dir,
            next_handle: 0,
            handles: HashMap::new(),
        }
    }

    fn path(&self, path: &str) -> PathBuf {
        if path == "~" {
            return self.home_dir.clone();
        }
        if let Some(path) = path.strip_prefix("~/") {
            return self.home_dir.join(path);
        }
        let path = Path::new(path);
        if path.is_absolute() {
            path.to_owned()
        } else {
            self.workdir.join(path)
        }
    }

    async fn canonical_path(&self, path: &str) -> io::Result<PathBuf> {
        let mut current = self.path(path);
        let mut missing = Vec::new();

        loop {
            match tokio::fs::canonicalize(&current).await {
                Ok(mut canonical) => {
                    for component in missing.iter().rev() {
                        canonical.push(component);
                    }
                    return Ok(canonical);
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    let Some(name) = current.file_name() else {
                        return Err(error);
                    };
                    missing.push(name.to_owned());
                    current.pop();
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn insert_handle(&mut self, value: OpenHandle) -> Result<String, StatusReply> {
        if self.handles.len() >= MAX_OPEN_HANDLES {
            return Err(StatusCode::Failure.with_message("too many open SFTP handles"));
        }

        loop {
            let handle = format!("{:016x}", self.next_handle);
            self.next_handle = self.next_handle.wrapping_add(1);
            if !self.handles.contains_key(&handle) {
                self.handles.insert(handle.clone(), value);
                return Ok(handle);
            }
        }
    }
}

impl Handler for Filesystem {
    type Error = StatusReply;

    fn unimplemented(&self) -> Self::Error {
        StatusCode::OpUnsupported.into()
    }

    async fn init(&mut self, _: u32, _: HashMap<String, String>) -> Result<Version, Self::Error> {
        let mut version = Version::new();
        version
            .extensions
            .insert(EXPAND_PATH.to_owned(), "1".to_owned());
        Ok(version)
    }

    async fn open(
        &mut self,
        id: u32,
        filename: String,
        flags: OpenFlags,
        attrs: FileAttributes,
    ) -> Result<Handle, Self::Error> {
        let path = self.path(&filename);

        let mut options = tokio::fs::OpenOptions::new();
        options
            .read(flags.contains(OpenFlags::READ))
            .write(flags.contains(OpenFlags::WRITE))
            .append(flags.contains(OpenFlags::APPEND))
            .truncate(flags.contains(OpenFlags::TRUNCATE));
        if flags.contains(OpenFlags::EXCLUDE) {
            options.create_new(true);
        } else {
            options.create(flags.contains(OpenFlags::CREATE));
        }

        let file = options.open(&path).await.map_err(sftp_error)?;

        if let Some(size) = attrs.size {
            file.set_len(size).await.map_err(sftp_error)?;
        }

        apply_path_attrs(&path, &attrs).await.map_err(sftp_error)?;

        let handle = self.insert_handle(OpenHandle::File { file, path })?;

        Ok(Handle { id, handle })
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
        let value = self.handles.remove(&handle).ok_or_else(invalid_handle)?;

        if let OpenHandle::File { mut file, .. } = value {
            file.flush().await.map_err(sftp_error)?;
        }

        Ok(ok(id))
    }

    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> Result<Data, Self::Error> {
        let OpenHandle::File { file, .. } =
            self.handles.get_mut(&handle).ok_or_else(invalid_handle)?
        else {
            return Err(invalid_handle());
        };

        file.seek(SeekFrom::Start(offset))
            .await
            .map_err(sftp_error)?;

        let mut data = vec![0; (len as usize).min(MAX_TRANSFER_SIZE)];
        let read = file.read(&mut data).await.map_err(sftp_error)?;

        if read == 0 {
            return Err(StatusCode::Eof.into());
        }

        data.truncate(read);
        Ok(Data { id, data })
    }

    async fn write(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<Status, Self::Error> {
        if data.len() > MAX_TRANSFER_SIZE {
            return Err(StatusCode::BadMessage.with_message("SFTP write exceeds transfer limit"));
        }
        let OpenHandle::File { file, .. } =
            self.handles.get_mut(&handle).ok_or_else(invalid_handle)?
        else {
            return Err(invalid_handle());
        };

        file.seek(SeekFrom::Start(offset))
            .await
            .map_err(sftp_error)?;

        file.write_all(&data).await.map_err(sftp_error)?;
        Ok(ok(id))
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        let metadata = tokio::fs::symlink_metadata(self.path(&path))
            .await
            .map_err(sftp_error)?;
        Ok(Attrs {
            id,
            attrs: attributes(&metadata),
        })
    }

    async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, Self::Error> {
        let metadata = match self.handles.get(&handle).ok_or_else(invalid_handle)? {
            OpenHandle::File { file, .. } => file.metadata().await,
            OpenHandle::Directory { path, .. } => tokio::fs::metadata(path).await,
        }
        .map_err(sftp_error)?;

        Ok(Attrs {
            id,
            attrs: attributes(&metadata),
        })
    }

    async fn setstat(
        &mut self,
        id: u32,
        path: String,
        attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        let path = self.path(&path);

        if let Some(size) = attrs.size {
            tokio::fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .await
                .map_err(sftp_error)?
                .set_len(size)
                .await
                .map_err(sftp_error)?;
        }

        apply_path_attrs(&path, &attrs).await.map_err(sftp_error)?;
        Ok(ok(id))
    }

    async fn fsetstat(
        &mut self,
        id: u32,
        handle: String,
        attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        let OpenHandle::File { file, path } =
            self.handles.get_mut(&handle).ok_or_else(invalid_handle)?
        else {
            return Err(invalid_handle());
        };

        if let Some(size) = attrs.size {
            file.set_len(size).await.map_err(sftp_error)?;
        }

        let path = path.clone();

        apply_path_attrs(&path, &attrs).await.map_err(sftp_error)?;
        Ok(ok(id))
    }

    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, Self::Error> {
        let path = self.path(&path);

        let entries = tokio::fs::read_dir(&path).await.map_err(sftp_error)?;

        let handle = self.insert_handle(OpenHandle::Directory { entries, path })?;
        Ok(Handle { id, handle })
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, Self::Error> {
        let OpenHandle::Directory { entries, .. } =
            self.handles.get_mut(&handle).ok_or_else(invalid_handle)?
        else {
            return Err(invalid_handle());
        };

        let mut files = Vec::with_capacity(DIRECTORY_BATCH_SIZE);
        while files.len() < DIRECTORY_BATCH_SIZE {
            let Some(entry) = entries.next_entry().await.map_err(sftp_error)? else {
                break;
            };
            let metadata = entry.metadata().await.map_err(sftp_error)?;
            files.push(File::new(
                entry.file_name().to_string_lossy(),
                attributes(&metadata),
            ));
        }

        if files.is_empty() {
            Err(StatusCode::Eof.into())
        } else {
            Ok(Name { id, files })
        }
    }

    async fn remove(&mut self, id: u32, filename: String) -> Result<Status, Self::Error> {
        tokio::fs::remove_file(self.path(&filename))
            .await
            .map_err(sftp_error)?;
        Ok(ok(id))
    }

    async fn mkdir(
        &mut self,
        id: u32,
        path: String,
        attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        let path = self.path(&path);

        tokio::fs::create_dir(&path).await.map_err(sftp_error)?;

        apply_path_attrs(&path, &attrs).await.map_err(sftp_error)?;
        Ok(ok(id))
    }

    async fn rmdir(&mut self, id: u32, path: String) -> Result<Status, Self::Error> {
        tokio::fs::remove_dir(self.path(&path))
            .await
            .map_err(sftp_error)?;
        Ok(ok(id))
    }

    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        let path = self.canonical_path(&path).await.map_err(sftp_error)?;

        Ok(Name {
            id,
            files: vec![File::dummy(path.to_string_lossy())],
        })
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        let metadata = tokio::fs::metadata(self.path(&path))
            .await
            .map_err(sftp_error)?;

        Ok(Attrs {
            id,
            attrs: attributes(&metadata),
        })
    }

    async fn rename(
        &mut self,
        id: u32,
        oldpath: String,
        newpath: String,
    ) -> Result<Status, Self::Error> {
        tokio::fs::rename(self.path(&oldpath), self.path(&newpath))
            .await
            .map_err(sftp_error)?;
        Ok(ok(id))
    }

    async fn readlink(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        let target = tokio::fs::read_link(self.path(&path))
            .await
            .map_err(sftp_error)?;

        Ok(Name {
            id,
            files: vec![File::dummy(target.to_string_lossy())],
        })
    }

    async fn symlink(
        &mut self,
        id: u32,
        linkpath: String,
        targetpath: String,
    ) -> Result<Status, Self::Error> {
        #[cfg(unix)]
        tokio::fs::symlink(targetpath, self.path(&linkpath))
            .await
            .map_err(sftp_error)?;
        #[cfg(not(unix))]
        {
            let _ = (linkpath, targetpath);
            return Err(StatusCode::OpUnsupported.into());
        }
        Ok(ok(id))
    }

    async fn extended(
        &mut self,
        id: u32,
        request: String,
        data: Vec<u8>,
    ) -> Result<Packet, Self::Error> {
        if request != EXPAND_PATH {
            return Err(StatusCode::OpUnsupported.into());
        }

        let mut data = Bytes::from(data);
        let request: ExpandPathExtension = russh_sftp::de::from_bytes(&mut data)
            .map_err(|error| StatusCode::BadMessage.with_message(error.to_string()))?;
        if !data.is_empty() {
            return Err(
                StatusCode::BadMessage.with_message("expand-path request contains trailing data")
            );
        }

        let path = self
            .canonical_path(&request.path)
            .await
            .map_err(sftp_error)?;
        Ok(Packet::Name(Name {
            id,
            files: vec![File::dummy(path.to_string_lossy())],
        }))
    }
}

pub(crate) async fn run(
    mut stream: impl AsyncRead + AsyncWrite + Unpin,
    mut handler: Filesystem,
) -> io::Result<()> {
    loop {
        let length = match stream.read_u32().await {
            Ok(length) => length,
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(error) => return Err(error),
        };
        if length > MAX_CLIENT_PACKET_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "SFTP packet length limit exceeded",
            ));
        }

        let mut bytes = vec![0; length as usize];
        stream.read_exact(&mut bytes).await?;
        let mut bytes = Bytes::from(bytes);
        let response = match Packet::try_from(&mut bytes) {
            Ok(request) => process_request(request, &mut handler).await,
            Err(_) => Packet::error(0, StatusCode::BadMessage),
        };
        let response = Bytes::try_from(response).map_err(io::Error::other)?;
        stream.write_all(&response).await?;
        stream.flush().await?;
    }
}

async fn process_request(packet: Packet, handler: &mut Filesystem) -> Packet {
    macro_rules! dispatch {
        ($id:expr, $request:expr, $method:ident; $($argument:ident),*) => {
            match handler.$method($($request.$argument),*).await {
                Err(error) => {
                    let StatusReply {
                        status_code,
                        error_message,
                        language_tag,
                    } = error;
                    Packet::Status(Status {
                        id: $id,
                        status_code,
                        error_message: error_message
                            .unwrap_or_else(|| status_code.to_string()),
                        language_tag: language_tag.unwrap_or_else(|| "en-US".to_owned()),
                    })
                }
                Ok(packet) => packet.into(),
            }
        };
    }

    let id = packet.get_request_id();
    match packet {
        Packet::Init(request) => dispatch!(id, request, init; version, extensions),
        Packet::Open(request) => dispatch!(id, request, open; id, filename, pflags, attrs),
        Packet::Close(request) => dispatch!(id, request, close; id, handle),
        Packet::Read(request) => dispatch!(id, request, read; id, handle, offset, len),
        Packet::Write(request) => dispatch!(id, request, write; id, handle, offset, data),
        Packet::Lstat(request) => dispatch!(id, request, lstat; id, path),
        Packet::Fstat(request) => dispatch!(id, request, fstat; id, handle),
        Packet::SetStat(request) => dispatch!(id, request, setstat; id, path, attrs),
        Packet::FSetStat(request) => dispatch!(id, request, fsetstat; id, handle, attrs),
        Packet::OpenDir(request) => dispatch!(id, request, opendir; id, path),
        Packet::ReadDir(request) => dispatch!(id, request, readdir; id, handle),
        Packet::Remove(request) => dispatch!(id, request, remove; id, filename),
        Packet::MkDir(request) => dispatch!(id, request, mkdir; id, path, attrs),
        Packet::RmDir(request) => dispatch!(id, request, rmdir; id, path),
        Packet::RealPath(request) => dispatch!(id, request, realpath; id, path),
        Packet::Stat(request) => dispatch!(id, request, stat; id, path),
        Packet::Rename(request) => dispatch!(id, request, rename; id, oldpath, newpath),
        Packet::ReadLink(request) => dispatch!(id, request, readlink; id, path),
        Packet::Symlink(request) => dispatch!(id, request, symlink; id, linkpath, targetpath),
        Packet::Extended(request) => dispatch!(id, request, extended; id, request, data),
        _ => Packet::error(0, StatusCode::BadMessage),
    }
}

fn ok(id: u32) -> Status {
    Status {
        id,
        status_code: StatusCode::Ok,
        error_message: "Ok".to_owned(),
        language_tag: "en-US".to_owned(),
    }
}

fn invalid_handle() -> StatusReply {
    StatusCode::Failure.with_message("invalid SFTP handle")
}

fn sftp_error(error: io::Error) -> StatusReply {
    let code = match error.kind() {
        io::ErrorKind::NotFound => StatusCode::NoSuchFile,
        io::ErrorKind::PermissionDenied => StatusCode::PermissionDenied,
        io::ErrorKind::InvalidData | io::ErrorKind::InvalidInput => StatusCode::BadMessage,
        _ => StatusCode::Failure,
    };
    code.with_message(error.to_string())
}

fn attributes(metadata: &std::fs::Metadata) -> FileAttributes {
    // `From<&Metadata>` serializes the raw `st_mode`, whose type bits are what
    // SFTP clients test with `S_ISREG`/`S_ISDIR`. Clearing russh-sftp type
    // flags is unsafe here: `FileMode::LNK` is the two-bit mask `0xA000`, so
    // `set_symlink(false)` also strips the regular-file bit `0x8000` and every
    // stat of a plain file reports an unknown type, breaking downloads.
    FileAttributes::from(metadata)
}

async fn apply_path_attrs(path: &Path, attrs: &FileAttributes) -> io::Result<()> {
    if let Some(mode) = attrs.permissions {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(mode & 0o7777))
                .await?;
        }
        #[cfg(not(unix))]
        {
            let mut permissions = tokio::fs::metadata(path).await?.permissions();
            permissions.set_readonly(mode & 0o200 == 0);
            tokio::fs::set_permissions(path, permissions).await?;
        }
    }

    if attrs.atime.is_some() || attrs.mtime.is_some() {
        let path = path.to_owned();
        let atime = attrs.atime;
        let mtime = attrs.mtime;

        tokio::task::spawn_blocking(move || {
            let file = std::fs::File::open(path)?;
            let mut times = std::fs::FileTimes::new();

            if let Some(atime) = atime {
                times = times.set_accessed(UNIX_EPOCH + Duration::from_secs(u64::from(atime)));
            }

            if let Some(mtime) = mtime {
                times = times.set_modified(UNIX_EPOCH + Duration::from_secs(u64::from(mtime)));
            }

            file.set_times(times)
        })
        .await
        .map_err(io::Error::other)??;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "axe-sftp-{}-{}",
                std::process::id(),
                NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&path);

            std::fs::create_dir(&path).expect("create SFTP scratch directory");

            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[tokio::test]
    async fn sftp_protocol_transfers_and_manages_files() {
        let scratch = Scratch::new();
        let (client_stream, server_stream) = tokio::io::duplex(1 << 20);
        let root = scratch.0.clone();
        let home = scratch.0.join("home");
        std::fs::create_dir(&home).expect("create SFTP home directory");

        let server =
            tokio::spawn(
                async move { run(server_stream, Filesystem::with_home(root, home)).await },
            );

        let session = russh_sftp::client::SftpSession::new(client_stream)
            .await
            .expect("initialize SFTP client");

        let mut file = session.create("upload.part").await.expect("create file");
        file.write_all(b"hello over sftp")
            .await
            .expect("write file");
        file.close().await.expect("close file");

        assert_eq!(
            session.read("upload.part").await.expect("read file"),
            b"hello over sftp"
        );
        let mut home_file = session
            .create("~/home-upload.part")
            .await
            .expect("create file in SFTP home");
        home_file
            .write_all(b"hello in home")
            .await
            .expect("write home file");
        home_file.close().await.expect("close home file");
        assert_eq!(
            std::fs::read(scratch.0.join("home/home-upload.part")).expect("read home file"),
            b"hello in home"
        );

        session
            .create_dir("directory")
            .await
            .expect("create directory");
        session
            .rename("upload.part", "directory/upload")
            .await
            .expect("rename file");
        let entries = session
            .read_dir("directory")
            .await
            .expect("read directory")
            .map(|entry| entry.file_name())
            .collect::<Vec<_>>();

        assert_eq!(entries, ["upload"]);
        session
            .remove_file("directory/upload")
            .await
            .expect("remove file");
        session
            .remove_dir("directory")
            .await
            .expect("remove directory");

        drop(session);

        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .expect("SFTP server did not stop")
            .expect("SFTP server task failed")
            .expect("SFTP server returned an error");
    }

    #[tokio::test]
    async fn expand_path_extension_resolves_session_home() {
        let scratch = Scratch::new();
        let home = scratch.0.join("home");
        std::fs::create_dir(&home).expect("create SFTP home directory");
        let mut filesystem = Filesystem::with_home(scratch.0.clone(), home.clone());
        let data: Vec<u8> = ExpandPathExtension {
            path: "~/new-file".to_owned(),
        }
        .try_into()
        .expect("encode expand-path request");

        let Packet::Name(response) = filesystem
            .extended(1, EXPAND_PATH.to_owned(), data)
            .await
            .expect("expand SFTP home path")
        else {
            panic!("expand-path must return a NAME response");
        };

        assert_eq!(
            response.files[0].filename,
            home.canonicalize()
                .expect("canonicalize SFTP home")
                .join("new-file")
                .to_string_lossy()
        );
    }

    #[tokio::test]
    async fn read_is_capped_to_one_bounded_response() {
        let scratch = Scratch::new();
        std::fs::write(scratch.0.join("large"), vec![b'x'; MAX_TRANSFER_SIZE + 1])
            .expect("write large fixture");
        let mut filesystem = Filesystem::with_home(scratch.0.clone(), scratch.0.clone());

        let handle = filesystem
            .open(
                1,
                "large".to_owned(),
                OpenFlags::READ,
                FileAttributes::default(),
            )
            .await
            .expect("open fixture")
            .handle;

        let response = filesystem
            .read(2, handle, 0, u32::MAX)
            .await
            .expect("read bounded response");

        assert_eq!(response.data.len(), MAX_TRANSFER_SIZE);
    }

    #[tokio::test]
    async fn rejects_handles_beyond_session_limit() {
        let scratch = Scratch::new();
        std::fs::write(scratch.0.join("file"), b"fixture").expect("write fixture");
        let mut filesystem = Filesystem::with_home(scratch.0.clone(), scratch.0.clone());

        for id in 0..MAX_OPEN_HANDLES {
            filesystem
                .open(
                    id as u32,
                    "file".to_owned(),
                    OpenFlags::READ,
                    FileAttributes::default(),
                )
                .await
                .expect("open handle within limit");
        }
        let error = filesystem
            .open(
                MAX_OPEN_HANDLES as u32,
                "file".to_owned(),
                OpenFlags::READ,
                FileAttributes::default(),
            )
            .await
            .expect_err("handle above limit must fail");

        assert_eq!(error.status_code, StatusCode::Failure);
    }

    #[cfg(unix)]
    #[test]
    fn sftp_attributes_preserve_file_type_bits() {
        let scratch = Scratch::new();
        let file_path = scratch.0.join("file");
        std::fs::write(&file_path, b"data").expect("write file");
        let dir_path = scratch.0.join("dir");
        std::fs::create_dir(&dir_path).expect("create dir");
        let link_path = scratch.0.join("link");
        std::os::unix::fs::symlink("file", &link_path).expect("create symlink");

        let file = attributes(&std::fs::metadata(&file_path).expect("stat file"));
        assert!(
            file.is_regular(),
            "regular file must stat as S_IFREG, got permissions {:o}",
            file.permissions.unwrap_or(0)
        );

        let dir = attributes(&std::fs::metadata(&dir_path).expect("stat dir"));
        assert!(
            dir.is_dir(),
            "directory must stat as S_IFDIR, got permissions {:o}",
            dir.permissions.unwrap_or(0)
        );

        let link = attributes(&std::fs::symlink_metadata(&link_path).expect("lstat symlink"));
        assert!(
            link.is_symlink(),
            "symlink must lstat as S_IFLNK, got permissions {:o}",
            link.permissions.unwrap_or(0)
        );
    }
}
