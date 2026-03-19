pub mod clip;

pub use clip::{Clip, ClipBank};

pub mod controller;

pub use controller::{SpriteController, Frame, TypeResolver, ValueResolver};

pub mod decls;

pub use decls::{SpriteDecl, SpriteId, Sprites};
