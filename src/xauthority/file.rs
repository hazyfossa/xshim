use super::encoding::{self, BinWrite, Entry};

use std::{
    fs::{File, OpenOptions},
    io,
    os::unix::fs::OpenOptionsExt,
    path::Path,
};

use lock::Lock;

pub struct AuthorityFile {
    file: File,
    _lock: Option<Lock>,
}

impl AuthorityFile {
    fn create_inner(path: &Path) -> io::Result<File> {
        OpenOptions::new()
            .read(true)
            .write(true)
            .mode(0o600)
            .create_new(true)
            .open(path)
    }

    pub fn create(path: &Path) -> io::Result<Self> {
        let file = Self::create_inner(path)?;
        let lock = Lock::aqquire(path)?;

        Ok(Self {
            file,
            _lock: Some(lock),
        })
    }

    /// # Safety
    /// the caller should ensure no other process will open the same path
    pub unsafe fn create_unlocked(path: &Path) -> io::Result<Self> {
        let file = Self::create_inner(path)?;
        Ok(Self { file, _lock: None })
    }

    pub fn set(&mut self, authority: impl IntoIterator<Item = Entry>) -> encoding::Result<()> {
        for entry in authority {
            entry.write(&mut self.file)?
        }

        Ok(())
    }
}

pub mod lock {
    use std::{
        fs::{OpenOptions, hard_link, remove_file},
        io,
        os::unix::fs::OpenOptionsExt,
        path::{Path, PathBuf},
    };

    fn replace_filename(mut path: PathBuf, new_filename: String) -> PathBuf {
        path.set_file_name(new_filename);
        path
    }

    // TODO: stale lock removal

    pub struct Lock {
        creat_path: PathBuf,
        link_path: PathBuf,
    }

    impl Lock {
        pub fn aqquire(xauth_path: &Path) -> io::Result<Self> {
            let filename = xauth_path.file_name().ok_or(io::Error::new(
                io::ErrorKind::InvalidFilename,
                "xauth_path does not end with a file",
            ))?;

            let filename = filename.to_str().ok_or(io::Error::new(
                io::ErrorKind::InvalidFilename,
                "xauth_path filename is not valid UTF-8",
            ))?;

            let creat_path = replace_filename(xauth_path.to_path_buf(), format!("{filename}-c"));
            // TODO: for full parity we need to handle the case where filesystem doesnt support hard links
            let link_path = replace_filename(xauth_path.to_path_buf(), format!("{filename}-l"));

            let lockfile = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&creat_path)?;

            drop(lockfile); // immediately close, as we don't need to interact with that file

            hard_link(&creat_path, &link_path)?;

            Ok(Self {
                creat_path,
                link_path,
            })
        }
    }

    impl Drop for Lock {
        fn drop(&mut self) {
            let _ = remove_file(&self.creat_path);
            let _ = remove_file(&self.link_path);
        }
    }
}
