//! vmg: the GTK front end to vm.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "widget positions, counts and fractions are small"
)]

mod actions;
mod application;
mod detail;
mod forms;
mod indicator;
mod model;
mod prefs;
mod tables;
mod terminals;
mod tree;
mod window;
mod worker;

use clap::Parser;
use gtk::glib;

#[derive(Debug, Parser)]
#[command(
    name = "vmg",
    version,
    about = "Manage QEMU virtual machines in a window"
)]
pub struct Cli {
    /// Do not publish a panel indicator; closing the window quits
    #[arg(long)]
    pub no_indicator: bool,
}

/// Reports a problem that does not stop the window.
pub fn warn(text: &str) {
    eprintln!("vmg: {text}");
}

fn main() -> glib::ExitCode {
    let cli = Cli::parse();
    #[allow(
        clippy::expect_used,
        reason = "nothing can be drawn without the resources"
    )]
    gtk::gio::resources_register_include!("vmg.gresource")
        .expect("the compiled-in resources should load");
    application::run(&cli)
}
