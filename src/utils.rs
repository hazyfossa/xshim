pub mod fd {
    use std::{ops::Range, os::fd::OwnedFd, process::Command};

    use anyhow::{Result, anyhow};
    use command_fds::{CommandFdExt, FdMapping};

    pub struct FdContext {
        free_fd_source: Range<u32>,
        mappings: Vec<FdMapping>,
    }

    impl FdContext {
        pub fn new(free_fd_source: Range<u32>) -> Self {
            Self {
                free_fd_source,
                mappings: Vec::new(),
            }
        }

        pub fn pass(&mut self, fd: OwnedFd) -> Result<PassedFd> {
            let mapped_fd = self
                .free_fd_source
                .next()
                .ok_or(anyhow!("Free fd source exhausted"))?;

            self.mappings.push(FdMapping {
                parent_fd: fd,
                child_fd: mapped_fd as i32,
            });
            Ok(PassedFd(mapped_fd))
        }
    }

    pub trait CommandFdCtxExt: CommandFdExt {
        fn with_fd_context(&mut self, fd_ctx: FdContext) -> &mut Self;
    }

    impl CommandFdCtxExt for Command {
        fn with_fd_context(&mut self, fd_ctx: FdContext) -> &mut Self {
            // if you see this error,
            // check if any manual mappings overlap with free_fd_source.
            self.fd_mappings(fd_ctx.mappings)
                .expect("Fd collision with context detected at runtime.")
        }
    }

    pub struct PassedFd(u32);

    impl PassedFd {
        // pub fn path(&self) -> PathBuf {
        //     PathBuf::from("/proc/self/fd/").join(self.0.to_string())
        // }

        pub fn num(&self) -> u32 {
            self.0
        }
    }
}
