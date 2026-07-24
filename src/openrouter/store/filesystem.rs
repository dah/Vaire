use super::*;

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(super) fn secure_directory(path: &Path) -> Result<(), OpenRouterStoreError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => return validate_directory(&metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Err(write_error()),
    }
    let parent = path.parent().ok_or_else(write_error)?;
    fs::create_dir_all(parent).map_err(|_| write_error())?;
    match DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err(write_error()),
    }
    let metadata = fs::symlink_metadata(path).map_err(|_| write_error())?;
    validate_directory(&metadata)
}

pub(super) fn validate_directory(metadata: &fs::Metadata) -> Result<(), OpenRouterStoreError> {
    if !metadata.file_type().is_dir()
        || metadata.uid() != current_uid()
        || metadata.mode() & 0o7777 != 0o700
    {
        return Err(permission_error());
    }
    Ok(())
}

pub(super) fn validate_file(metadata: &fs::Metadata) -> Result<(), OpenRouterStoreError> {
    if !metadata.file_type().is_file()
        || metadata.uid() != current_uid()
        || metadata.mode() & 0o7777 != 0o600
    {
        return Err(permission_error());
    }
    Ok(())
}

pub(super) fn read_file_limited(
    path: &Path,
    limit: usize,
) -> Result<Vec<u8>, OpenRouterStoreError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(OpenRouterStoreError::new(
                OpenRouterStoreFailureCategory::NotFound,
            ));
        }
        Err(_) => return Err(read_error()),
    };
    validate_file(&metadata)?;
    if metadata.len() > limit as u64 {
        return Err(limit_error());
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| read_error())?;
    validate_file(&file.metadata().map_err(|_| read_error())?)?;
    let mut bytes = Vec::new();
    file.take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| read_error())?;
    if bytes.len() > limit {
        return Err(limit_error());
    }
    Ok(bytes)
}

pub(super) fn write_atomic(
    directory: &Path,
    target: &Path,
    bytes: &[u8],
    limit: usize,
    directory_sync: &dyn DirectorySync,
) -> Result<CommitStatus, OpenRouterStoreError> {
    if bytes.len() > limit {
        return Err(limit_error());
    }
    validate_directory(&fs::symlink_metadata(directory).map_err(|_| write_error())?)?;
    match fs::symlink_metadata(target) {
        Ok(metadata) => validate_file(&metadata)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Err(write_error()),
    }
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(write_error)?;
    let mut temporary = None;
    for _ in 0..128 {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = directory.join(format!(".{name}.{}.{}.tmp", std::process::id(), sequence));
        match OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(file) => {
                temporary = Some((path, file));
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(write_error()),
        }
    }
    let (temporary_path, mut file) = temporary.ok_or_else(write_error)?;
    let precommit = (|| {
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|_| write_error())?;
        file.write_all(bytes).map_err(|_| write_error())?;
        file.sync_all().map_err(|_| write_error())?;
        fs::rename(&temporary_path, target).map_err(|_| write_error())?;
        Ok(())
    })();
    if let Err(error) = precommit {
        let _ = fs::remove_file(&temporary_path);
        return Err(error);
    }
    Ok(match directory_sync.sync(directory) {
        Ok(()) => CommitStatus::Verified,
        Err(_) if fs::read(target).is_ok_and(|written| written == bytes) => {
            CommitStatus::CommittedUnverified
        }
        Err(_) => return Err(write_error()),
    })
}

pub(super) fn aggregate_conversation_bytes(directory: &Path) -> Result<u64, OpenRouterStoreError> {
    let mut total = 0u64;
    for entry in fs::read_dir(directory).map_err(|_| read_error())? {
        let entry = entry.map_err(|_| read_error())?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name == "index.json" || !name.starts_with("or_") || !name.ends_with(".json") {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path()).map_err(|_| read_error())?;
        if metadata.file_type().is_file() {
            total = total.saturating_add(metadata.len());
        }
    }
    Ok(total)
}

pub(super) fn current_uid() -> u32 {
    // SAFETY: geteuid has no preconditions and retains no pointers.
    unsafe { libc::geteuid() }
}

pub(super) fn read_error() -> OpenRouterStoreError {
    OpenRouterStoreError::new(OpenRouterStoreFailureCategory::Read)
}

pub(super) fn write_error() -> OpenRouterStoreError {
    OpenRouterStoreError::new(OpenRouterStoreFailureCategory::Write)
}

pub(super) fn delete_error() -> OpenRouterStoreError {
    OpenRouterStoreError::new(OpenRouterStoreFailureCategory::Delete)
}

pub(super) fn permission_error() -> OpenRouterStoreError {
    OpenRouterStoreError::new(OpenRouterStoreFailureCategory::Permissions)
}

pub(super) fn corrupt_error() -> OpenRouterStoreError {
    OpenRouterStoreError::new(OpenRouterStoreFailureCategory::Corrupt)
}

pub(super) fn unsupported_version_error() -> OpenRouterStoreError {
    OpenRouterStoreError::new(OpenRouterStoreFailureCategory::UnsupportedVersion)
}

pub(super) fn limit_error() -> OpenRouterStoreError {
    OpenRouterStoreError::new(OpenRouterStoreFailureCategory::ResourceLimit)
}
