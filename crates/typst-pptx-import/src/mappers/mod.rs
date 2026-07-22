//! One module per PowerPoint construct, each turning PML into Typst IR.
//!
//! Every mapping *judgement* lives here — what a preset shape becomes, when a
//! bullet is Typst's own, which of a picture's two blips to prefer. The
//! parser below has no opinions and the emitter above has none either.

pub mod picture;
pub mod shape;
pub mod table;
pub mod text;
