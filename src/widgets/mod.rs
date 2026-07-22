//! Lua-declared, live-updating widgets (see `Documentation.md`'s "Widgets"
//! section in the `bread` repo) rendered into breadbar's fixed layout slots.

pub mod client;
mod render;

pub use render::build_node;
