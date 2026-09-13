//! Application setup: actions, styling and the single window.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::Cli;
use crate::window::VmgWindow;

pub const APP_ID: &str = "com.paperstack.VmManager";
pub const RESOURCE_PATH: &str = "/com/paperstack/VmManager";
pub const APP_NAME: &str = "VM Manager";

pub fn run(cli: &Cli) -> glib::ExitCode {
    let application = adw::Application::builder().application_id(APP_ID).build();
    application.connect_startup(|application| {
        setup_actions(application);
        load_style();
    });
    let want_indicator = !cli.no_indicator;
    let existing: Rc<RefCell<Option<VmgWindow>>> = Rc::new(RefCell::new(None));
    application.connect_shutdown(glib::clone!(
        #[strong]
        existing,
        move |_| {
            if let Some(window) = existing.borrow().as_ref() {
                window.store_layout();
            }
        }
    ));
    application.connect_activate(move |application| {
        if let Some(window) = existing.borrow().as_ref() {
            window.present();
            return;
        }
        let window = VmgWindow::new(application);
        window.start(want_indicator);
        window.present();
        existing.replace(Some(window));
    });
    // clap has parsed the command line, so GApplication must not.
    application.run_with_args(&["vmg"])
}

fn setup_actions(application: &adw::Application) {
    let quit = gtk::gio::SimpleAction::new("quit", None);
    quit.connect_activate(glib::clone!(
        #[weak]
        application,
        move |_, _| application.quit()
    ));
    application.add_action(&quit);
    application.set_accels_for_action("app.quit", &["<Control>q"]);
    application.set_accels_for_action("win.show-sidebar", &["F9"]);
    application.set_accels_for_action("win.refresh", &["F5"]);
    application.set_accels_for_action("win.run", &["<Control>n"]);

    let about = gtk::gio::SimpleAction::new("about", None);
    about.connect_activate(glib::clone!(
        #[weak]
        application,
        move |_, _| show_about(&application)
    ));
    application.add_action(&about);
}

fn show_about(application: &adw::Application) {
    let dialog = adw::AboutDialog::builder()
        .application_name(APP_NAME)
        .application_icon(APP_ID)
        .version(env!("CARGO_PKG_VERSION"))
        .developer_name("Dave Minter")
        .comments("Create and manage QEMU virtual machines.")
        .license_type(gtk::License::MitX11)
        .build();
    dialog.present(application.active_window().as_ref());
}

fn load_style() {
    let provider = gtk::CssProvider::new();
    provider.load_from_resource(&format!("{RESOURCE_PATH}/style.css"));
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        gtk::IconTheme::for_display(&display).add_resource_path(&format!("{RESOURCE_PATH}/icons"));
    }
}
