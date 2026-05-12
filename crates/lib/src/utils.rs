pub mod fd {

    use std::{
        mem::ManuallyDrop,
        os::{
            fd::{AsRawFd, OwnedFd, RawFd},
            unix::process::CommandExt,
        },
        path::PathBuf,
        process::Command,
    };

    use rustix::io::FdFlags;

    /// A context should be the only thing that maps FDs for the specific command
    pub struct FdContext {
        parent_fds: ManuallyDrop<Vec<OwnedFd>>,
        is_current: bool,
    }

    // Drop only if not set on current, otherwise preserve fds
    impl Drop for FdContext {
        fn drop(&mut self) {
            if !self.is_current {
                unsafe {
                    ManuallyDrop::drop(&mut self.parent_fds);
                }
            }
        }
    }

    impl FdContext {
        pub fn new() -> Self {
            Self {
                parent_fds: ManuallyDrop::new(Vec::new()),
                is_current: false,
            }
        }

        pub fn pass(&mut self, fd: OwnedFd) -> PassedFd {
            let ret = PassedFd(fd.as_raw_fd());
            self.parent_fds.push(fd);
            ret
        }
    }

    // TODO: this can only be safely called once
    pub trait CommandFdExt {
        fn with_fd_context(&mut self, ctx: FdContext) -> &mut Self;
    }

    impl CommandFdExt for Command {
        fn with_fd_context(&mut self, mut ctx: FdContext) -> &mut Self {
            // Safety: the closure not allocate, rustix calls are safe
            unsafe {
                self.pre_exec(move || {
                    for fd in &*ctx.parent_fds {
                        // Remove the FD_CLOEXEC flag, so the FD will be kept open after exec.
                        rustix::io::fcntl_setfd(&fd, FdFlags::empty())?;
                    }

                    ctx.is_current = true;

                    Ok(())
                })
            };

            self
        }
    }

    pub struct PassedFd(RawFd);

    impl AsRawFd for PassedFd {
        fn as_raw_fd(&self) -> RawFd {
            self.0
        }
    }

    impl PassedFd {
        pub fn path(&self) -> PathBuf {
            PathBuf::from("/proc/self/fd/").join(self.0.to_string())
        }
    }
}

pub mod subprocess {
    use std::{os::unix::process::CommandExt, process::Command};

    use rustix::process::{Signal, set_parent_process_death_signal};

    pub trait CleanupExt {
        fn with_cleanup(&mut self) -> &mut Self;
    }

    impl CleanupExt for Command {
        fn with_cleanup(&mut self) -> &mut Self {
            // Safety: does not allocate, rustix call is safe
            unsafe {
                self.pre_exec(|| {
                    set_parent_process_death_signal(Some(Signal::KILL))?;
                    Ok(())
                })
            }
        }
    }
}

pub mod runtime_dir {
    use std::{fs, ops::Deref, os::unix::fs::PermissionsExt, path::PathBuf};

    use envy::define_env;
    use eyre::{Context, Result, ensure};

    #[derive(Debug)]
    pub struct RuntimeDir {
        path: PathBuf,
    }

    impl Deref for RuntimeDir {
        type Target = PathBuf;
        fn deref(&self) -> &Self::Target {
            &self.path
        }
    }

    define_env!(pub RuntimeDirEnv(PathBuf) = #raw "XDG_RUNTIME_DIR");

    impl RuntimeDir {
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
    }
}

pub mod private_file {
    use std::os::fd::OwnedFd;

    use rustix::{
        fs::{MemfdFlags, SealFlags, fcntl_add_seals, memfd_create},
        io::{Errno, write},
    };

    pub struct PrivateFile(OwnedFd);

    impl PrivateFile {
        pub fn new(name: &str) -> Result<Self, Errno> {
            let memfd = memfd_create(name, MemfdFlags::ALLOW_SEALING)?;
            Ok(Self(memfd))
        }

        pub fn seal(self) -> Result<SealedPrivateFile, Errno> {
            fcntl_add_seals(&self.0, SealFlags::all())?;
            Ok(SealedPrivateFile(self.0))
        }
    }

    impl std::io::Write for PrivateFile {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            Ok(write(&self.0, buf)?)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    pub struct SealedPrivateFile(OwnedFd);

    impl SealedPrivateFile {
        pub fn into_inner(self) -> OwnedFd {
            self.0
        }
    }
}
