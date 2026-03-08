#[macro_export]
macro_rules! trait_alias {
    ($vis:vis trait $name:ident = $($for:tt)*) => {
        $vis trait $name: $($for)* {}
        impl<T: $($for)*> $name for T {}
    };
}

pub mod fd {
    // Copyright 2021, The Android Open Source Project
    // Copyright 2026, hazyfossa
    //
    // This code is based on the command-fds crate.
    // Main changes are:
    // 1) fd mappings are mostly valid by construction, we only resolve conflicts at runtime.
    // 2) system calls are over rustix instead of nix
    //
    // Original license notice below:
    //
    // Licensed under the Apache License, Version 2.0 (the "License");
    // you may not use this file except in compliance with the License.
    // You may obtain a copy of the License at
    //
    //     http://www.apache.org/licenses/LICENSE-2.0
    //
    // Unless required by applicable law or agreed to in writing, software
    // distributed under the License is distributed on an "AS IS" BASIS,
    // WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
    // See the License for the specific language governing permissions and
    // limitations under the License.
    use std::{
        io, mem,
        ops::RangeInclusive,
        os::{
            fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
            unix::process::CommandExt,
        },
        process::Command,
    };

    use anyhow::{Result, anyhow};
    use rustix::io::FdFlags;

    trait_alias!(pub trait FdSource = Iterator<Item = RawFd> + Send + Sync + 'static);

    struct FdMapping {
        parent_fd: OwnedFd,
        child_fd: RawFd,
    }

    pub struct FdContext<T> {
        free_fd_source: T,
        mappings: Vec<FdMapping>,
    }

    pub type SimpleFdContext = FdContext<RangeInclusive<RawFd>>;

    impl FdContext<RangeInclusive<RawFd>> {
        pub fn new(capacity: i32) -> Self {
            let capacity = capacity.checked_add(1).unwrap_or_else(|| {
                panic!(
                    "overflow: the limit for SimpleFdContext capacity is {}",
                    i32::MAX - 1
                )
            });

            Self::manual(3..=capacity)
        }
    }

    impl<T: FdSource> FdContext<T> {
        /// free_fd_source should be an iterator yielding valid, unused FDs in the child.
        ///
        /// If you aren't doing any FD passing besides FdContext, specifying any range
        /// beyond 0..=2 should be safe.
        ///
        /// Note that the range should always contain one more fd than you want to pass.
        /// It will be used for reassigning in case of conflict.
        /// Failure
        pub fn manual(free_fd_source: T) -> Self {
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

        /// This function does not allocate
        fn apply(&mut self) -> io::Result<()> {
            // NOTE: mappings are valid by linear construction from iterator

            let safe_temporary_fd = self.free_fd_source.next().expect(
                "Cannot assign a safe temporary fd: free_fd_source exhausted.
                Potential conflict resolution will fail. Expand free_fd_source.",
            );

            let child_fds: Vec<RawFd> = self.mappings.iter().map(|m| m.child_fd).collect();

            // Resolve conflicts between parent and child
            for mapping in self.mappings.iter_mut() {
                if child_fds.contains(&mapping.parent_fd.as_raw_fd())
                    && mapping.parent_fd.as_raw_fd() != mapping.child_fd
                {
                    mapping.parent_fd =
                        rustix::io::fcntl_dupfd_cloexec(&mapping.parent_fd, safe_temporary_fd)?;
                }
            }

            for mapping in &self.mappings {
                if mapping.child_fd == mapping.parent_fd.as_raw_fd() {
                    // Remove the FD_CLOEXEC flag, so the FD will be kept open after exec.
                    rustix::io::fcntl_setfd(&mapping.parent_fd, FdFlags::empty())?;
                } else {
                    // This closes child_fd if it is already open as something else, and clears the
                    // FD_CLOEXEC flag on child_fd.

                    // Safety:
                    // fds from free_fd_source are guaranteed (by caller) to be unused
                    // child_fd in each mapping is derived from free_fd_source
                    // therefore, we have permission to treat any child_fd as Owned,
                    // and close it if necessary
                    let mut owned_projection = unsafe { OwnedFd::from_raw_fd(mapping.child_fd) };
                    rustix::io::dup2(&mapping.parent_fd, &mut owned_projection)?;
                    mem::forget(owned_projection);
                }
            }

            Ok(())
        }
    }

    pub trait CommandFdExt {
        fn with_fd_context<T: FdSource>(&mut self, fd_ctx: FdContext<T>) -> &mut Self;
    }

    impl CommandFdExt for Command {
        fn with_fd_context<T: FdSource>(&mut self, mut fd_ctx: FdContext<T>) -> &mut Self {
            // Safety: apply() does not allocate, rustix calls are safe
            unsafe { self.pre_exec(move || fd_ctx.apply()) }
        }
    }

    pub struct PassedFd(RawFd);

    impl AsRawFd for PassedFd {
        fn as_raw_fd(&self) -> RawFd {
            self.0
        }
    }

    impl PassedFd {
        // pub fn path(&self) -> PathBuf {
        //     PathBuf::from("/proc/self/fd/").join(self.0.to_string())
        // }
    }
}

pub mod subprocess {
    use std::{
        io,
        ops::{Deref, DerefMut},
        os::unix::process::CommandExt,
        process::{Child, Command},
    };

    use rustix::process::{Pid, Signal, kill_process, set_parent_process_death_signal};

    pub struct ChildWithCleanup(Child);

    impl Deref for ChildWithCleanup {
        type Target = Child;
        fn deref(&self) -> &Self::Target {
            &self.0
        }
    }

    impl DerefMut for ChildWithCleanup {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.0
        }
    }

    impl Drop for ChildWithCleanup {
        fn drop(&mut self) {
            let ret = kill_process(Pid::from_child(&self.0), Signal::TERM);

            if ret.is_ok() {
                let _best_effort = self.0.wait();
            }
        }
    }

    pub fn spawn_with_cleanup(command: &mut Command) -> Result<ChildWithCleanup, io::Error> {
        // Safety: does not allocate, rustix call is safe
        let command = unsafe {
            command.pre_exec(|| {
                set_parent_process_death_signal(Some(Signal::KILL))?;
                Ok(())
            })
        };
        let child = command.spawn()?;
        Ok(ChildWithCleanup(child))
    }
}

pub mod warn {
    use crate::systemd::journald::{self, LogLevel};

    // TODO: zero-alloc with format_args
    // note that it may be impossible (journald encoding requires us to check for \n)
    // which already necessitates some sort of string lookup before we even started writing
    #[macro_export]
    macro_rules! warn {
        ($($tt:tt)?) => {
            let _ = journald::log(LogLevel::Warning, &format!($($tt)?));
        };
    }

    pub trait WarnExt<T> {
        fn warn(self) -> Option<T>;
    }

    impl<T> WarnExt<T> for anyhow::Result<T> {
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
