#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunFailure {
    Protocol(String),
    Provider(String),
    Store(String),
    Client(String),
}

impl RunFailure {
    pub fn category(&self) -> &'static str {
        match self {
            Self::Protocol(_) => "protocol",
            Self::Provider(_) => "provider",
            Self::Store(_) => "store",
            Self::Client(_) => "client",
        }
    }
}

impl From<crate::Error> for RunFailure {
    fn from(error: crate::Error) -> Self {
        use crate::Error;
        match error {
            Error::Protocol(message) | Error::Config(message) => Self::Protocol(message),
            Error::Provider(message) => Self::Provider(message),
            Error::Store(message) => Self::Store(message),
            Error::Cancelled => Self::Client("run was cancelled".into()),
            Error::Http(error) => Self::Provider(error.to_string()),
            Error::Database(error) => Self::Store(error.to_string()),
            Error::Migration(error) => Self::Store(error.to_string()),
            Error::Io(error) => Self::Store(error.to_string()),
            Error::Decode(error) => Self::Protocol(error.to_string()),
            Error::Encode(error) => Self::Protocol(error.to_string()),
            Error::Json(error) => Self::Protocol(error.to_string()),
            Error::RunNotFound(run_id) => Self::Store(format!("run not found: {run_id}")),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunOutcome {
    Completed,
    Cancelled,
    Failed(RunFailure),
}
