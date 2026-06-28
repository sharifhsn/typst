//! The per-element mapper modules. Each lowers one native element family into
//! the DOCX IR. The foundation provides compiling stubs; mapper agents fill
//! exactly one module each, keeping the public handler signatures identical.

pub mod footnote;
pub mod heading;
pub mod image;
pub mod list;
pub mod math;
pub mod outline;
pub mod reference;
pub mod shape;
pub mod table;
