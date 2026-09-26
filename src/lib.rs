//! keepane: a tmux-like terminal multiplexer for Windows (ConPTY, PowerShell, WSL).

pub mod client;
pub mod clipboard;
pub mod command;
pub mod completion;
pub mod config;
pub mod console;
pub mod format;
pub mod histlog;
pub mod ipc;
pub mod keys;
pub mod legacy;
pub mod logger;
pub mod notify;
pub mod pager;
pub mod proccwd;
pub mod resurrect;
pub mod server;
pub mod shutdown;
pub mod startup;
pub mod sysinfo;
pub mod update;
pub mod web;
pub mod winsec;
pub mod wt;
