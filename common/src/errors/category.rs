use std::fmt::{self, Display, Formatter};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCategory {
    UserInput,
    Config,
    Crypto,
    Rpc,
    Storage,
    Transaction,
    Sync,
}

impl ErrorCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UserInput => "UserInput",
            Self::Config => "Config",
            Self::Crypto => "Crypto",
            Self::Rpc => "Rpc",
            Self::Storage => "Storage",
            Self::Transaction => "Transaction",
            Self::Sync => "Sync",
        }
    }
}

impl Display for ErrorCategory {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
