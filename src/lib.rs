//! VeilForge: an easy file and folder locker for Windows.
//!
//! Nothing is encrypted: items are protected with ordinary NTFS features
//! (ACLs, same-volume renames into a holding folder). Only the map that
//! remembers where everything belongs is encrypted with the master password.
//!
//! The logic lives in this library so that it can be tested; `main.rs` only
//! parses the command line.
pub mod attempts;
pub mod cli;
pub mod demo;
pub mod emergency;
pub mod engine;
pub mod model;
pub mod openflow;
pub mod secret;
pub mod selftest;
pub mod settings;
pub mod shellmenu;
pub mod standin;
pub mod strength;
pub mod sys;
pub mod ui;
pub mod util;
