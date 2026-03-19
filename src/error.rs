use snafu::FromString;

// Default error type
pub type Result<T, E = snafu::Whatever> = std::result::Result<T, E>;

// Prelude
pub use snafu::whatever;
pub use warn::WarnExt;

// Shortcuts
// TODO: adapt trait alias to this

pub trait OptionCtx<T>: snafu::OptionExt<T> {
    fn ctx<S, E>(self, context: S) -> Result<T, E>
    where
        S: Into<String>,
        E: FromString,
    {
        self.whatever_context(context)
    }
}

impl<T, All: snafu::OptionExt<T>> OptionCtx<T> for All {}

pub trait ResultCtx<T, E>: snafu::ResultExt<T, E> {
    fn ctx<S, E2>(self, context: S) -> Result<T, E2>
    where
        S: Into<String>,
        E2: FromString,
        E: Into<E2::Source>,
    {
        self.whatever_context(context)
    }
}

impl<T, E, All: snafu::ResultExt<T, E>> ResultCtx<T, E> for All {}

//

pub mod warn {
    // TODO: zero-alloc with format_args
    // note that it may be impossible (journald encoding requires us to check for \n)
    // which already necessitates some sort of string lookup before we even started writing
    #[macro_export]
    macro_rules! warn {
        ($($tt:tt)?) => {
            let _ = $crate::systemd::journald::log($
                crate::systemd::journald::LogLevel::Warning,
                &format!($($tt)?)
            );
        };
    }

    pub trait WarnExt<T> {
        fn warn(self) -> Option<T>;
    }

    impl<T> WarnExt<T> for super::Result<T> {
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
