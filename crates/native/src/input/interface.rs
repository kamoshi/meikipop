use std::error::Error;

pub trait PointerProvider: Send {
    fn position(&mut self) -> Result<(i32, i32), Box<dyn Error>>;
}
