use std::error::Error;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ConnectionExt, Window};
use x11rb::rust_connection::RustConnection;

use crate::input::interface::PointerProvider;

pub struct X11PointerProvider {
    connection: RustConnection,
    root: Window,
}

impl X11PointerProvider {
    pub fn new() -> Result<Self, Box<dyn Error>> {
        let (connection, screen_number) = x11rb::connect(None)?;
        let root = connection.setup().roots[screen_number].root;
        Ok(Self { connection, root })
    }
}

impl PointerProvider for X11PointerProvider {
    fn position(&mut self) -> Result<(i32, i32), Box<dyn Error>> {
        let pointer = self.connection.query_pointer(self.root)?.reply()?;
        Ok((i32::from(pointer.root_x), i32::from(pointer.root_y)))
    }
}
