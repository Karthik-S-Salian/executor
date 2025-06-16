use std::{error::Error, fmt};

#[derive(Debug)]
pub struct StringError(String);

impl StringError {
    pub fn new(e:&str)->StringError{
        StringError(e.to_string())
    }
}

impl fmt::Display for StringError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Error for StringError {}

