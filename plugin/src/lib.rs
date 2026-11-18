//! The host's audio as a Yoke Plugin, through PipeWire.
//!
//! `graph` folds what PipeWire describes into a picture, `backend` reaches PipeWire and knows nothing of
//! Yoke, and `plugin` is the Plugin around them.

pub mod backend;
pub mod graph;
pub mod plugin;
