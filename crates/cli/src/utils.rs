pub mod path {
    use std::path::Path;

    use eyre::{Result, ensure};

    pub trait EnsureExistsExt: Sized {
        fn ensure_exists(self) -> Result<Self>;
    }

    impl<T> EnsureExistsExt for T
    where
        T: AsRef<Path>,
    {
        fn ensure_exists(self) -> Result<Self> {
            let this = self.as_ref();
            ensure!(this.exists(), "the path {} does not exist", this.display());
            Ok(self)
        }
    }
}

pub mod send_fds {
    use std::{
        io::IoSlice,
        os::{
            fd::{AsFd, BorrowedFd},
            unix::net::{UnixDatagram, UnixStream},
        },
    };

    use rustix::{
        io,
        net::{SendAncillaryBuffer, SendAncillaryMessage, SendFlags, sendmsg},
    };

    pub trait SendFds: AsFd {
        // TODO: is it sensible to restrict to Owned?
        fn send_fds(&self, fds: &[BorrowedFd]) -> io::Result<usize> {
            let mut anc = SendAncillaryBuffer::new(&mut []);
            anc.push(SendAncillaryMessage::ScmRights(fds));

            // Send a single null byte, as a true empty message won't be processed by peer
            let empty = IoSlice::new(b"\0");

            sendmsg(self.as_fd(), &[empty], &mut anc, SendFlags::empty())
        }
    }

    impl SendFds for UnixDatagram {}
    impl SendFds for UnixStream {}
}

pub mod warn {
    use std::fmt::Debug;

    // TODO: zero-alloc with format_args
    // note that it may be impossible (journald encoding requires us to check for \n)
    // which already necessitates some sort of string lookup before we even started writing
    #[macro_export]
    macro_rules! warn {
        ($($tt:tt)*) => {
            let _ = $crate::systemd::journald::log($
                crate::systemd::journald::LogLevel::Warning,
                &format!($($tt)?)
            );
        };
    }

    pub trait WarnExt<T> {
        fn warn(self) -> Option<T>;
    }

    impl<T, E: Debug> WarnExt<T> for std::result::Result<T, E> {
        fn warn(self) -> Option<T> {
            match self {
                Ok(value) => Some(value),
                Err(e) => {
                    warn!("{e:?}");
                    None
                }
            }
        }
    }
}

pub mod runtime_dir {
    use std::{
        fs::{self, DirBuilder, remove_dir_all},
        ops::Deref,
        os::unix::fs::{DirBuilderExt, PermissionsExt},
        path::PathBuf,
    };

    use envy::define_env;
    use eyre::{Context, Result, ensure};

    pub struct RuntimeDir {
        pub path: PathBuf,
    }

    impl Deref for RuntimeDir {
        type Target = PathBuf;

        fn deref(&self) -> &Self::Target {
            &self.path
        }
    }

    impl Drop for RuntimeDir {
        fn drop(&mut self) {
            let _ = remove_dir_all(&self.path);
        }
    }

    #[derive(Debug)]
    pub struct RuntimeDirManager {
        path: PathBuf,
    }

    define_env!(pub RuntimeDirEnv(PathBuf) = #raw "XDG_RUNTIME_DIR");

    impl RuntimeDirManager {
        pub fn from_env(env: &impl envy::Get) -> Result<Self> {
            let path = env
                .get::<RuntimeDirEnv>()
                .context("Environment does not provide a runtime directory")?
                .0;

            let permissions = fs::metadata(&path)
                .context("Cannot query runtime dir metadata. Does it exist?")?
                .permissions()
                .mode();

            ensure!(
                permissions & 0o077 == 0,
                "Runtime directory is insecure: expecting permissions `077`, got {permissions}"
            );

            Ok(Self { path })
        }

        pub fn create(&self, name: &str) -> Result<RuntimeDir> {
            let directory = self.path.join(name);

            DirBuilder::new()
                .mode(0o700)
                .create(&directory)
                .context(format!("cannot create path: {directory:?}"))?;

            Ok(RuntimeDir { path: directory })
        }
    }
}
