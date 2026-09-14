//! Dialogs that gather what a command needs, and the parsing behind them.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use vm_core::catalogue::Login;
use vm_core::compression::Compression;
use vm_core::config::Key;
use vm_core::configuration::{Action, Added, Removed};
use vm_core::instance::{Instance, Port, Share};
use vm_core::machine::{Chipset, Disk, Firmware};
use vm_core::machines::{Changes, PruneTarget, Pull, Request};
use vm_core::settings;

use crate::model::{Host, Snapshot};
use crate::worker::{Command, Export, Import};

/// The names the firmware, chipset and disk choices are offered under.
pub const FIRMWARE: [&str; 3] = ["image default", "bios", "uefi"];
pub const CHIPSET: [&str; 3] = ["image default", "q35", "pc"];
pub const DISK: [&str; 4] = ["image default", "virtio", "sata", "ide"];
pub const PULL: [&str; 4] = ["config default", "missing", "always", "never"];
pub const SSH_CONFIG: [&str; 3] = ["config default", "yes", "no"];
pub const COMPRESSION: [&str; 4] = ["none", "xz", "gzip", "zstd"];
const PORTS_TITLE: &str = "Forwarded ports, as host:guest or address:host:guest";
const VOLUMES_TITLE: &str = "Shared directories, one host:guest or host:guest:ro per line";

/// Everything the run dialog collects, as text and indices.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunFields {
    pub reference: String,
    pub name: String,
    pub memory: String,
    pub cpus: u32,
    pub ports: String,
    pub user: String,
    pub volumes: String,
    pub disk_size: String,
    pub pull: usize,
    pub firmware: usize,
    pub cpu_model: String,
    pub machine: usize,
    pub disk: usize,
    pub password: String,
    pub ssh_config: usize,
    pub auto_remove: bool,
}

fn blank(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

fn firmware_of(index: usize) -> Result<Option<Firmware>, String> {
    FIRMWARE
        .get(index)
        .filter(|_| index > 0)
        .map(|name| settings::parse_firmware(name))
        .transpose()
}

fn chipset_of(index: usize) -> Result<Option<Chipset>, String> {
    CHIPSET
        .get(index)
        .filter(|_| index > 0)
        .map(|name| settings::parse_machine(name))
        .transpose()
}

fn disk_of(index: usize) -> Result<Option<Disk>, String> {
    DISK.get(index)
        .filter(|_| index > 0)
        .map(|name| settings::parse_disk(name))
        .transpose()
}

/// Ports as `[address:]host:guest`, separated by commas, spaces or lines.
pub fn parse_ports(text: &str) -> Result<Vec<Port>, String> {
    text.split(|character: char| character == ',' || character.is_whitespace())
        .filter(|piece| !piece.is_empty())
        .map(settings::parse_port)
        .collect()
}

/// Shares as `host:guest[:ro]`, one per line.
pub fn parse_shares(text: &str) -> Result<Vec<Share>, String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(settings::parse_share)
        .collect()
}

fn hashed(password: &str) -> Result<Option<String>, String> {
    if password.is_empty() {
        return Ok(None);
    }
    vm_core::crypt::hash(password)
        .map(Some)
        .map_err(|error| error.to_string())
}

const fn toggle_of(index: usize) -> Option<bool> {
    match index {
        1 => Some(true),
        2 => Some(false),
        _ => None,
    }
}

pub fn run_request(fields: &RunFields) -> Result<Request, String> {
    let reference = blank(&fields.reference).ok_or("an image is needed")?;
    Ok(Request {
        reference,
        name: blank(&fields.name),
        memory: settings::parse_memory(if fields.memory.trim().is_empty() {
            "2G"
        } else {
            &fields.memory
        })?,
        cpus: fields.cpus.max(1),
        ports: parse_ports(&fields.ports)?,
        user: blank(&fields.user)
            .map(|user| settings::parse_user(&user))
            .transpose()?,
        shares: parse_shares(&fields.volumes)?,
        disk_size: blank(&fields.disk_size),
        pull: match fields.pull {
            1 => Some(Pull::Missing),
            2 => Some(Pull::Always),
            3 => Some(Pull::Never),
            _ => None,
        },
        firmware: firmware_of(fields.firmware)?,
        cpu_model: blank(&fields.cpu_model)
            .map(|cpu| settings::parse_cpu_model(&cpu))
            .transpose()?,
        machine: chipset_of(fields.machine)?,
        disk: disk_of(fields.disk)?,
        password: hashed(&fields.password)?,
        ssh_config: toggle_of(fields.ssh_config),
        auto_remove: fields.auto_remove,
    })
}

/// The settings dialog's fields, compared against the machine to find the changes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SettingsFields {
    pub memory: String,
    pub cpus: u32,
    pub ports: String,
    pub user: String,
    pub volumes: String,
    pub disk_size: String,
    pub firmware: usize,
    pub cpu_model: String,
    pub machine: usize,
    pub disk: usize,
    pub password: String,
    pub remove_password: bool,
    pub ssh_config: bool,
    pub eject: bool,
}

impl SettingsFields {
    /// The dialog as it opens: the machine's own settings.
    pub fn of(held: &Instance) -> Self {
        Self {
            memory: format!("{}M", held.memory),
            cpus: held.cpus,
            ports: held
                .ports
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", "),
            user: held.user.clone(),
            volumes: held
                .shares
                .iter()
                .map(Share::spec)
                .collect::<Vec<_>>()
                .join("\n"),
            disk_size: String::new(),
            firmware: index_of(&FIRMWARE, held.firmware.name()),
            cpu_model: held.cpu_model.clone(),
            machine: index_of(&CHIPSET, held.machine.name()),
            disk: index_of(&DISK, held.disk.name()),
            password: String::new(),
            remove_password: false,
            ssh_config: held.ssh_config,
            eject: false,
        }
    }
}

fn index_of(names: &[&str], name: &str) -> usize {
    names.iter().position(|held| *held == name).unwrap_or(0)
}

pub fn start_changes(held: &Instance, fields: &SettingsFields) -> Result<Changes, String> {
    let memory = settings::parse_memory(&fields.memory)?;
    let ports = parse_ports(&fields.ports)?;
    let shares = parse_shares(&fields.volumes)?;
    let same_shares = shares.len() == held.shares.len()
        && shares.iter().zip(&held.shares).all(|(new, old)| {
            new.source == old.source && new.target == old.target && new.readonly == old.readonly
        });
    let user = blank(&fields.user).ok_or("a user name is needed")?;
    let firmware = firmware_of(fields.firmware)?;
    let machine = chipset_of(fields.machine)?;
    let disk = disk_of(fields.disk)?;
    let cpu = blank(&fields.cpu_model)
        .map(|cpu| settings::parse_cpu_model(&cpu))
        .transpose()?;
    Ok(Changes {
        memory: (memory != held.memory).then_some(memory),
        cpus: (fields.cpus.max(1) != held.cpus).then(|| fields.cpus.max(1)),
        ports: (ports != held.ports).then_some(ports),
        shares: (!same_shares).then_some(shares),
        user: (user != held.user).then_some(user),
        disk_size: blank(&fields.disk_size),
        firmware: firmware.filter(|wanted| *wanted != held.firmware),
        cpu_model: cpu.filter(|wanted| *wanted != held.cpu_model),
        machine: machine.filter(|wanted| *wanted != held.machine),
        disk: disk.filter(|wanted| *wanted != held.disk),
        password: if fields.remove_password {
            Some("*".to_owned())
        } else {
            hashed(&fields.password)?
        },
        ssh_config: (fields.ssh_config != held.ssh_config).then_some(fields.ssh_config),
        eject: fields.eject,
    })
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportFields {
    pub source: String,
    pub reference: String,
    pub description: String,
    pub login: usize,
    pub arch: usize,
    pub firmware: usize,
    pub cpu_model: String,
    pub machine: usize,
    pub disk: usize,
    pub digest: String,
    pub forget_url: bool,
    pub force: bool,
}

pub fn import_request(fields: &ImportFields) -> Result<Import, String> {
    let source = blank(&fields.source).ok_or("a file or URL is needed")?;
    settings::parse_source(&source)?;
    let reference = blank(&fields.reference).ok_or("a name for the image is needed")?;
    let description = blank(&fields.description)
        .map(|text| settings::parse_description(&text))
        .transpose()?;
    let digest = blank(&fields.digest)
        .map(|text| settings::parse_digest(&text).map(|_| text))
        .transpose()?;
    let arch = vm_core::ARCHITECTURES
        .get(fields.arch)
        .ok_or("an architecture is needed")?;
    Ok(Import {
        source,
        reference,
        arch: (*arch).to_owned(),
        description,
        login: *Login::ALL.get(fields.login).unwrap_or(&Login::None),
        hardware: vm_core::import::Hardware {
            firmware: firmware_of(fields.firmware)?.unwrap_or_default(),
            cpu_model: blank(&fields.cpu_model)
                .map(|cpu| settings::parse_cpu_model(&cpu))
                .transpose()?,
            machine: chipset_of(fields.machine)?.unwrap_or_default(),
            disk: disk_of(fields.disk)?.unwrap_or_default(),
        },
        digest,
        fetchable: !fields.forget_url,
        force: fields.force,
    })
}

pub fn export_request(
    reference: &str,
    file: Option<&PathBuf>,
    arch: &str,
    compression: usize,
    force: bool,
) -> Result<Export, String> {
    let file = file.ok_or("choose where to write the image")?.clone();
    let compression = match compression {
        0 => Compression::None,
        index => settings::parse_compression(COMPRESSION.get(index).copied().unwrap_or("xz"))?,
    };
    Ok(Export {
        reference: reference.to_owned(),
        file,
        arch: arch.to_owned(),
        compression,
        force,
    })
}

/// The copy dialog's fields: which way, and the two paths.
pub fn copy_command(
    name: &str,
    to_guest: bool,
    host: Option<&PathBuf>,
    guest: &str,
) -> Result<Command, String> {
    let host = host.ok_or("choose a file or directory on this host")?;
    let guest = blank(guest).ok_or("a path in the guest is needed")?;
    let guest = format!("{name}:{guest}");
    let host = host.display().to_string();
    Ok(if to_guest {
        Command::Copy {
            from: host,
            to: guest,
        }
    } else {
        Command::Copy {
            from: guest,
            to: host,
        }
    })
}

/// A dialog with a page of rows, a Cancel button and one that confirms.
pub struct Form {
    pub dialog: adw::Dialog,
    pub page: adw::PreferencesPage,
    banner: adw::Banner,
    confirm: gtk::Button,
}

impl Form {
    pub fn new(title: &str, confirm: &str, destructive: bool) -> Self {
        let dialog = adw::Dialog::builder()
            .title(title)
            .content_width(560)
            .content_height(640)
            .build();
        let header = adw::HeaderBar::builder()
            .show_start_title_buttons(false)
            .show_end_title_buttons(false)
            .build();
        let cancel = gtk::Button::with_label("Cancel");
        cancel.connect_clicked(glib::clone!(
            #[weak]
            dialog,
            move |_| {
                dialog.close();
            }
        ));
        header.pack_start(&cancel);
        let confirm_button = gtk::Button::with_label(confirm);
        confirm_button.add_css_class(if destructive {
            "destructive-action"
        } else {
            "suggested-action"
        });
        header.pack_end(&confirm_button);
        let banner = adw::Banner::new("");
        let page = adw::PreferencesPage::new();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&banner);
        content.append(&page);
        let view = adw::ToolbarView::builder().content(&content).build();
        view.add_top_bar(&header);
        dialog.set_child(Some(&view));
        dialog.set_default_widget(Some(&confirm_button));
        Self {
            dialog,
            page,
            banner,
            confirm: confirm_button,
        }
    }

    pub fn group(&self, title: &str) -> adw::PreferencesGroup {
        let group = adw::PreferencesGroup::builder().title(title).build();
        self.page.add(&group);
        group
    }

    /// Shows the dialog; a confirmation that fails shows why and stays open.
    pub fn present(
        self,
        parent: &impl IsA<gtk::Widget>,
        on_confirm: impl Fn() -> Result<Vec<Command>, String> + 'static,
        sink: Rc<dyn Fn(Command)>,
    ) {
        let dialog = self.dialog.clone();
        let banner = self.banner.clone();
        self.confirm.connect_clicked(move |_| match on_confirm() {
            Ok(commands) => {
                for command in commands {
                    sink(command);
                }
                dialog.close();
            }
            Err(reason) => {
                banner.set_title(&reason);
                banner.set_revealed(true);
            }
        });
        self.dialog.present(Some(parent));
    }
}

pub fn entry(group: &adw::PreferencesGroup, title: &str, text: &str) -> adw::EntryRow {
    let row = adw::EntryRow::builder().title(title).text(text).build();
    group.add(&row);
    row
}

pub fn password(group: &adw::PreferencesGroup, title: &str) -> adw::PasswordEntryRow {
    let row = adw::PasswordEntryRow::builder().title(title).build();
    group.add(&row);
    row
}

pub fn switch(
    group: &adw::PreferencesGroup,
    title: &str,
    subtitle: &str,
    active: bool,
) -> adw::SwitchRow {
    let row = adw::SwitchRow::builder()
        .title(title)
        .subtitle(subtitle)
        .active(active)
        .build();
    group.add(&row);
    row
}

pub fn spin(
    group: &adw::PreferencesGroup,
    title: &str,
    range: (f64, f64),
    value: f64,
) -> adw::SpinRow {
    let row = adw::SpinRow::with_range(range.0, range.1, 1.0);
    row.set_title(title);
    row.set_value(value);
    group.add(&row);
    row
}

pub fn combo(
    group: &adw::PreferencesGroup,
    title: &str,
    options: &[&str],
    selected: usize,
) -> adw::ComboRow {
    let row = adw::ComboRow::builder()
        .title(title)
        .model(&gtk::StringList::new(options))
        .selected(selected as u32)
        .build();
    group.add(&row);
    row
}

/// A multi-line text entry inside a group.
pub fn lines(group: &adw::PreferencesGroup, title: &str, text: &str) -> gtk::TextView {
    let view = gtk::TextView::builder()
        .monospace(true)
        .accepts_tab(false)
        .top_margin(6)
        .bottom_margin(6)
        .left_margin(6)
        .right_margin(6)
        .build();
    view.buffer().set_text(text);
    view.update_property(&[gtk::accessible::Property::Label(title)]);
    let frame = gtk::Frame::builder().child(&view).build();
    frame.add_css_class("view");
    let row = adw::PreferencesRow::builder()
        .title(title)
        .child(&frame)
        .activatable(false)
        .build();
    let labelled = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let label = gtk::Label::builder().label(title).xalign(0.0).build();
    label.add_css_class("dim-label");
    labelled.append(&label);
    labelled.append(&row);
    group.add(&labelled);
    view
}

pub fn text_of(view: &gtk::TextView) -> String {
    let buffer = view.buffer();
    buffer
        .text(&buffer.start_iter(), &buffer.end_iter(), false)
        .to_string()
}

/// What a file chooser row picks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    File,
    Folder,
    Save,
}

/// A row whose button opens a file dialog, holding what was chosen.
pub fn file_row(
    group: &adw::PreferencesGroup,
    title: &str,
    pick: Pick,
    initial_name: Option<String>,
) -> (adw::ActionRow, Rc<RefCell<Option<PathBuf>>>) {
    let row = adw::ActionRow::builder()
        .title(title)
        .subtitle("none chosen")
        .use_markup(false)
        .build();
    let button = gtk::Button::from_icon_name(match pick {
        Pick::Folder => "folder-symbolic",
        Pick::File | Pick::Save => "document-open-symbolic",
    });
    button.set_valign(gtk::Align::Center);
    button.update_property(&[gtk::accessible::Property::Label("Choose")]);
    row.add_suffix(&button);
    let chosen: Rc<RefCell<Option<PathBuf>>> = Rc::new(RefCell::new(None));
    let held = chosen.clone();
    button.connect_clicked(glib::clone!(
        #[weak]
        row,
        move |button| {
            let dialog = gtk::FileDialog::builder().modal(true).build();
            if let Some(name) = &initial_name {
                dialog.set_initial_name(Some(name));
            }
            let window = button.root().and_downcast::<gtk::Window>();
            let held = held.clone();
            let done = move |result: Result<gtk::gio::File, glib::Error>| {
                if let Ok(file) = result
                    && let Some(path) = file.path()
                {
                    row.set_subtitle(&path.display().to_string());
                    held.replace(Some(path));
                }
            };
            match pick {
                Pick::File => dialog.open(window.as_ref(), gtk::gio::Cancellable::NONE, done),
                Pick::Folder => {
                    dialog.select_folder(window.as_ref(), gtk::gio::Cancellable::NONE, done);
                }
                Pick::Save => dialog.save(window.as_ref(), gtk::gio::Cancellable::NONE, done),
            }
        }
    ));
    group.add(&row);
    (row, chosen)
}

fn hardware_rows(
    form: &Form,
    firmware: usize,
    cpu_model: &str,
    machine: usize,
    disk: usize,
) -> (adw::ComboRow, adw::EntryRow, adw::ComboRow, adw::ComboRow) {
    let group = form.group("Hardware");
    (
        combo(&group, "Firmware", &FIRMWARE, firmware),
        entry(&group, "CPU model", cpu_model),
        combo(&group, "Chipset", &CHIPSET, machine),
        combo(&group, "Disk controller", &DISK, disk),
    )
}

/// The run dialog, submitting a `Run` command.
pub fn run_dialog(
    parent: &impl IsA<gtk::Widget>,
    reference: &str,
    host: &Host,
    sink: Rc<dyn Fn(Command)>,
) {
    let form = Form::new("Run a Machine", "Run", false);
    let group = form.group("Machine");
    let image = entry(&group, "Image", reference);
    let name = entry(&group, "Name", "");
    let user = entry(&group, "User", "");
    user.set_show_apply_button(false);
    let password = password(&group, "Console password");
    let ssh_config = combo(&group, "Add an SSH config entry", &SSH_CONFIG, 0);
    let pull = combo(&group, "Fetch the image", &PULL, 0);
    let auto_remove = switch(
        &group,
        "Remove once stopped",
        "Deletes the machine and its disk when it stops",
        false,
    );
    let sizes = form.group("Size");
    let memory = entry(&sizes, "Memory", "2G");
    let cpus = spin(&sizes, "Processors", (1.0, 255.0), 2.0);
    let disk_size = entry(&sizes, "Disk size", "");
    let sharing = form.group("Sharing");
    let ports = entry(&sharing, PORTS_TITLE, "");
    let volumes = lines(&sharing, VOLUMES_TITLE, "");
    let (firmware, cpu, machine, disk) = hardware_rows(&form, 0, "", 0, 0);
    let _ = host;
    form.present(
        parent,
        move || {
            let fields = RunFields {
                reference: image.text().to_string(),
                name: name.text().to_string(),
                memory: memory.text().to_string(),
                cpus: cpus.value() as u32,
                ports: ports.text().to_string(),
                user: user.text().to_string(),
                volumes: text_of(&volumes),
                disk_size: disk_size.text().to_string(),
                pull: pull.selected() as usize,
                firmware: firmware.selected() as usize,
                cpu_model: cpu.text().to_string(),
                machine: machine.selected() as usize,
                disk: disk.selected() as usize,
                password: password.text().to_string(),
                ssh_config: ssh_config.selected() as usize,
                auto_remove: auto_remove.is_active(),
            };
            Ok(vec![Command::Run(Box::new(run_request(&fields)?))])
        },
        sink,
    );
}

/// The settings dialog for a stopped machine, submitting a `Start` with its changes.
pub fn settings_dialog(
    parent: &impl IsA<gtk::Widget>,
    held: &Instance,
    start: bool,
    sink: Rc<dyn Fn(Command)>,
) {
    let fields = SettingsFields::of(held);
    let form = Form::new(
        &format!("Settings for {}", held.name),
        if start { "Start" } else { "Apply" },
        false,
    );
    let group = form.group("Access");
    let user = entry(&group, "User", &fields.user);
    let password = password(&group, "New console password");
    let remove_password = switch(&group, "Remove the console password", "", false);
    let ssh_config = switch(
        &group,
        "SSH config entry",
        "Lets plain ssh reach the machine by name",
        fields.ssh_config,
    );
    let sizes = form.group("Size");
    let memory = entry(&sizes, "Memory", &fields.memory);
    let cpus = spin(&sizes, "Processors", (1.0, 255.0), f64::from(fields.cpus));
    let disk_size = entry(&sizes, "Grow the disk to", "");
    let sharing = form.group("Sharing");
    let ports = entry(&sharing, PORTS_TITLE, &fields.ports);
    let volumes = lines(&sharing, VOLUMES_TITLE, &fields.volumes);
    let (firmware, cpu, machine, disk) = hardware_rows(
        &form,
        fields.firmware,
        &fields.cpu_model,
        fields.machine,
        fields.disk,
    );
    let eject = held.cdrom.as_ref().map(|path| {
        let media = form.group("Media");
        switch(
            &media,
            "Eject the CD-ROM",
            &path.display().to_string(),
            false,
        )
    });
    let held = held.clone();
    let name = held.name.clone();
    form.present(
        parent,
        move || {
            let fields = SettingsFields {
                memory: memory.text().to_string(),
                cpus: cpus.value() as u32,
                ports: ports.text().to_string(),
                user: user.text().to_string(),
                volumes: text_of(&volumes),
                disk_size: disk_size.text().to_string(),
                firmware: firmware.selected() as usize,
                cpu_model: cpu.text().to_string(),
                machine: machine.selected() as usize,
                disk: disk.selected() as usize,
                password: password.text().to_string(),
                remove_password: remove_password.is_active(),
                ssh_config: ssh_config.is_active(),
                eject: eject.as_ref().is_some_and(adw::SwitchRow::is_active),
            };
            Ok(vec![Command::Start {
                name: name.clone(),
                changes: Box::new(start_changes(&held, &fields)?),
            }])
        },
        sink,
    );
}

pub fn clone_dialog(
    parent: &impl IsA<gtk::Widget>,
    name: &str,
    running: bool,
    sink: Rc<dyn Fn(Command)>,
) {
    let form = Form::new(&format!("Clone {name}"), "Clone", false);
    let group = form.group("New image");
    let image = entry(&group, "Image name, as name:tag", &format!("{name}:latest"));
    let description = entry(&group, "Description", "");
    let force = running.then(|| {
        switch(
            &group,
            "Copy without pausing",
            "Unflushed writes are missing from the clone",
            false,
        )
    });
    let name = name.to_owned();
    form.present(
        parent,
        move || {
            let target = blank(&image.text()).ok_or("a name for the image is needed")?;
            let description = blank(&description.text())
                .map(|text| settings::parse_description(&text))
                .transpose()?;
            Ok(vec![Command::Clone {
                name: name.clone(),
                target,
                description,
                force: force.as_ref().is_some_and(adw::SwitchRow::is_active),
            }])
        },
        sink,
    );
}

pub fn stop_dialog(parent: &impl IsA<gtk::Widget>, name: &str, sink: Rc<dyn Fn(Command)>) {
    let form = Form::new(&format!("Stop {name}"), "Stop", false);
    let group = form.group("Shutdown");
    let timeout = spin(
        &group,
        "Seconds to wait before forcing",
        (1.0, 3600.0),
        30.0,
    );
    let name = name.to_owned();
    form.present(
        parent,
        move || {
            Ok(vec![Command::Stop {
                name: name.clone(),
                timeout: timeout.value() as u64,
                force: false,
            }])
        },
        sink,
    );
}

/// The run-command dialog's fields.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExecFields {
    /// A command line for the guest's `sh`.
    pub command: String,
    pub workdir: String,
    /// `KEY=VALUE`, one per line.
    pub env: String,
    pub root: bool,
}

/// A command run in a terminal tab, which forwards typing and gives it a terminal.
pub fn exec_request(fields: &ExecFields) -> Result<vm_core::access::Exec, String> {
    let command = blank(&fields.command).ok_or("a command is needed")?;
    let env = fields
        .env
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(settings::parse_env)
        .collect::<Result<Vec<_>, String>>()?;
    Ok(vm_core::access::Exec {
        command: vec!["sh".to_owned(), "-c".to_owned(), command],
        interactive: true,
        tty: true,
        env,
        workdir: blank(&fields.workdir),
        root: fields.root,
    })
}

pub fn exec_dialog(parent: &impl IsA<gtk::Widget>, name: &str, sink: Rc<dyn Fn(Command)>) {
    let form = Form::new(&format!("Run a Command on {name}"), "Run", false);
    let group = form.group("Command");
    let command = entry(&group, "Command line", "");
    let workdir = entry(&group, "Working directory", "");
    let root = switch(&group, "Run as root", "Through sudo, or doas", false);
    let wait = spin(
        &group,
        "Seconds to wait for the machine to accept its key",
        (0.0, 3600.0),
        60.0,
    );
    let environment = form.group("Environment");
    let env = lines(&environment, "Variables, one KEY=VALUE per line", "");
    let name = name.to_owned();
    form.present(
        parent,
        move || {
            let fields = ExecFields {
                command: command.text().to_string(),
                workdir: workdir.text().to_string(),
                env: text_of(&env),
                root: root.is_active(),
            };
            Ok(vec![Command::Exec {
                name: name.clone(),
                request: Box::new(exec_request(&fields)?),
                wait: std::time::Duration::from_secs(wait.value() as u64),
            }])
        },
        sink,
    );
}

pub fn copy_dialog(parent: &impl IsA<gtk::Widget>, name: &str, sink: Rc<dyn Fn(Command)>) {
    let form = Form::new(&format!("Copy files with {name}"), "Copy", false);
    let group = form.group("Direction");
    let direction = combo(
        &group,
        "Copy",
        &[
            "from this host into the guest",
            "from the guest onto this host",
        ],
        0,
    );
    let paths = form.group("Paths");
    let (_, host) = file_row(&paths, "On this host", Pick::File, None);
    let folder = gtk::Button::with_label("Choose a directory instead");
    folder.set_halign(gtk::Align::Start);
    let (folder_row, host_folder) =
        file_row(&paths, "Or a directory on this host", Pick::Folder, None);
    let _ = (folder, folder_row);
    let guest = entry(&paths, "Path in the guest", "");
    let name = name.to_owned();
    form.present(
        parent,
        move || {
            let chosen = host
                .borrow()
                .clone()
                .or_else(|| host_folder.borrow().clone());
            Ok(vec![copy_command(
                &name,
                direction.selected() == 0,
                chosen.as_ref(),
                &guest.text(),
            )?])
        },
        sink,
    );
}

pub fn import_dialog(parent: &impl IsA<gtk::Widget>, host: &Host, sink: Rc<dyn Fn(Command)>) {
    let form = Form::new("Import an Image", "Import", false);
    let group = form.group("Source");
    let source = entry(&group, "File or URL", "");
    let (_, picked) = file_row(&group, "Or choose a file", Pick::File, None);
    let digest = entry(&group, "Expected sha512 digest", "");
    let forget = switch(
        &group,
        "Keep only the local copy",
        "A URL is otherwise fetched again after pruning",
        false,
    );
    let naming = form.group("Image");
    let reference = entry(&naming, "Name, as name:tag", "");
    let description = entry(&naming, "Description", "");
    let login = combo(&naming, "Guest access", &["cloud-init", "console only"], 1);
    let arch = combo(
        &naming,
        "Architecture",
        &vm_core::ARCHITECTURES,
        index_of(&vm_core::ARCHITECTURES, &host.arch),
    );
    let force = switch(&naming, "Replace an image of the same name", "", false);
    let (firmware, cpu, machine, disk) = hardware_rows(&form, 0, "", 0, 0);
    form.present(
        parent,
        move || {
            let typed = source.text().to_string();
            let source = if typed.trim().is_empty() {
                picked
                    .borrow()
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default()
            } else {
                typed
            };
            let fields = ImportFields {
                source,
                reference: reference.text().to_string(),
                description: description.text().to_string(),
                login: login.selected() as usize,
                arch: arch.selected() as usize,
                firmware: firmware.selected() as usize,
                cpu_model: cpu.text().to_string(),
                machine: machine.selected() as usize,
                disk: disk.selected() as usize,
                digest: digest.text().to_string(),
                forget_url: forget.is_active(),
                force: force.is_active(),
            };
            Ok(vec![Command::Import(Box::new(import_request(&fields)?))])
        },
        sink,
    );
}

pub fn export_dialog(
    parent: &impl IsA<gtk::Widget>,
    reference: &str,
    architectures: &[String],
    host_arch: &str,
    sink: Rc<dyn Fn(Command)>,
) {
    let form = Form::new(&format!("Export {reference}"), "Export", false);
    let group = form.group("Destination");
    let stem = reference.replace([':', '/'], "-");
    let (_, file) = file_row(&group, "Write to", Pick::Save, Some(stem));
    let compression = combo(&group, "Compress with", &COMPRESSION, 0);
    let force = switch(&group, "Replace an existing file", "", false);
    let names: Vec<&str> = architectures.iter().map(String::as_str).collect();
    let arch = combo(&group, "Architecture", &names, index_of(&names, host_arch));
    let architectures = architectures.to_vec();
    let reference = reference.to_owned();
    form.present(
        parent,
        move || {
            let arch = architectures
                .get(arch.selected() as usize)
                .ok_or("an architecture is needed")?;
            Ok(vec![Command::Export(Box::new(export_request(
                &reference,
                file.borrow().as_ref(),
                arch,
                compression.selected() as usize,
                force.is_active(),
            )?))])
        },
        sink,
    );
}

/// Asks what to prune, then lists what a dry run found before removing it.
pub fn prune_dialog(parent: &impl IsA<gtk::Widget>, sink: Rc<dyn Fn(Command)>) {
    let form = Form::new("Prune", "List", false);
    let group = form.group("What to remove");
    let target = combo(
        &group,
        "Prune",
        &["machines and images", "machines", "images"],
        0,
    );
    let all = switch(
        &group,
        "Everything unused",
        "Every stopped machine and every image no machine uses",
        false,
    );
    form.present(
        parent,
        move || {
            Ok(vec![Command::Prune {
                target: match target.selected() {
                    1 => Some(PruneTarget::Machines),
                    2 => Some(PruneTarget::Images),
                    _ => None,
                },
                all: all.is_active(),
                dry_run: true,
            }])
        },
        sink,
    );
}

/// The configuration dialog: settings applied on confirm, catalogues changed as they go.
pub fn configuration_dialog(
    parent: &impl IsA<gtk::Widget>,
    snapshot: &Snapshot,
    sink: Rc<dyn Fn(Command)>,
) {
    let host = &snapshot.host;
    let form = Form::new("Configuration", "Apply", false);
    let group = form.group("Settings");
    let auto_pull = switch(
        &group,
        "Fetch missing images when running",
        "auto_pull",
        host.auto_pull,
    );
    let add_ssh_config = switch(
        &group,
        "Add SSH config entries for new machines",
        "add_ssh_config",
        host.add_ssh_config,
    );
    let default_user = entry(&group, "Default user, or $USER", &host.default_user);
    let remotes = form.group("Remote catalogues");
    let locals = form.group("Local catalogues");
    list_catalogues(&remotes, &locals, snapshot, &sink);
    let adding = form.group("Add a remote catalogue");
    let remote_name = entry(&adding, "Name", "");
    let remote_url = entry(&adding, "URL of a catalogue pointer file", "");
    let remote_path = entry(&adding, "Directory within the archive", "");
    let remote_before = entry(&adding, "Place before", "");
    let adding_local = form.group("Add a local catalogue");
    let local_name = entry(&adding_local, "Name", "");
    let (_, local_path) = file_row(&adding_local, "Directory", Pick::Folder, None);
    let local_before = entry(&adding_local, "Place before", "");
    let before = (
        host.auto_pull,
        host.add_ssh_config,
        host.default_user.clone(),
    );
    form.present(
        parent,
        move || {
            let mut commands = Vec::new();
            let mut setting = |key: Key, value: String, unchanged: bool| {
                if !unchanged {
                    commands.push(Command::Configure(Box::new(Action::Set { key, value })));
                }
            };
            setting(
                Key::AutoPull,
                auto_pull.is_active().to_string(),
                auto_pull.is_active() == before.0,
            );
            setting(
                Key::AddSshConfig,
                add_ssh_config.is_active().to_string(),
                add_ssh_config.is_active() == before.1,
            );
            let user = default_user.text().to_string();
            match blank(&user) {
                Some(user) => setting(Key::DefaultUser, user.clone(), user == before.2),
                None => commands.push(Command::Configure(Box::new(Action::Unset {
                    key: Key::DefaultUser,
                }))),
            }
            if let Some(name) = blank(&remote_name.text()) {
                let url = blank(&remote_url.text()).ok_or("a remote catalogue needs a URL")?;
                commands.push(Command::Configure(Box::new(Action::Add(Added::Remote {
                    name,
                    url,
                    path: blank(&remote_path.text()),
                    before: blank(&remote_before.text()),
                }))));
            }
            if let Some(name) = blank(&local_name.text()) {
                let path = local_path
                    .borrow()
                    .clone()
                    .ok_or("a local catalogue needs a directory")?;
                commands.push(Command::Configure(Box::new(Action::Add(Added::Local {
                    name,
                    path,
                    before: blank(&local_before.text()),
                }))));
            }
            Ok(commands)
        },
        sink,
    );
}

/// Lists each catalogue with a button that removes it at once.
fn list_catalogues(
    remotes: &adw::PreferencesGroup,
    locals: &adw::PreferencesGroup,
    snapshot: &Snapshot,
    sink: &Rc<dyn Fn(Command)>,
) {
    for source in &snapshot.catalogues {
        if source.name == vm_core::config::STORE_CATALOGUE {
            continue;
        }
        let (group, removal) = match source.kind {
            vm_core::catalogue::Kind::Remote => (
                remotes,
                Removed::Remote {
                    name: source.name.clone(),
                },
            ),
            vm_core::catalogue::Kind::Local => (
                locals,
                Removed::Local {
                    name: source.name.clone(),
                },
            ),
        };
        let row = adw::ActionRow::builder()
            .title(&source.name)
            .subtitle(source.directory.display().to_string())
            .use_markup(false)
            .build();
        if source.kind == vm_core::catalogue::Kind::Remote {
            let update = gtk::Button::from_icon_name("view-refresh-symbolic");
            update.set_valign(gtk::Align::Center);
            update.set_tooltip_text(Some("Fetch this catalogue again"));
            update.update_property(&[gtk::accessible::Property::Label("Update catalogue")]);
            let (sink, name) = (sink.clone(), source.name.clone());
            update.connect_clicked(move |_| {
                sink(Command::Update {
                    named: Some(name.clone()),
                });
            });
            row.add_suffix(&update);
        }
        let remove = gtk::Button::from_icon_name("user-trash-symbolic");
        remove.set_valign(gtk::Align::Center);
        remove.update_property(&[gtk::accessible::Property::Label("Remove catalogue")]);
        let sink = sink.clone();
        remove.connect_clicked(glib::clone!(
            #[weak]
            row,
            move |_| {
                row.set_sensitive(false);
                sink(Command::Configure(Box::new(Action::Remove(
                    removal.clone(),
                ))));
            }
        ));
        row.add_suffix(&remove);
        group.add(&row);
    }
}

/// A yes-or-no question before something is removed.
pub fn confirm(
    parent: &impl IsA<gtk::Widget>,
    heading: &str,
    body: &str,
    verb: &str,
    items: &[String],
    on_confirm: impl Fn() + 'static,
) {
    let dialog = adw::AlertDialog::new(Some(heading), Some(body));
    dialog.add_responses(&[("cancel", "Cancel"), ("confirm", verb)]);
    dialog.set_response_appearance("confirm", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    if !items.is_empty() {
        let list = gtk::Box::new(gtk::Orientation::Vertical, 2);
        for item in items {
            let label = gtk::Label::builder()
                .label(item)
                .xalign(0.0)
                .selectable(true)
                .build();
            label.add_css_class("monospace");
            list.append(&label);
        }
        let scroller = gtk::ScrolledWindow::builder()
            .child(&list)
            .max_content_height(240)
            .propagate_natural_height(true)
            .build();
        dialog.set_extra_child(Some(&scroller));
    }
    dialog.connect_response(None, move |_, response| {
        if response == "confirm" {
            on_confirm();
        }
    });
    dialog.present(Some(parent));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_request_takes_defaults_for_what_is_left_blank() {
        let request = run_request(&RunFields {
            reference: "debian:trixie".to_owned(),
            ..RunFields::default()
        })
        .unwrap_or_else(|reason| panic!("{reason}"));
        assert_eq!(request.memory, 2048);
        assert_eq!(request.cpus, 1);
        assert_eq!(request.pull, None);
        assert_eq!(request.ssh_config, None);
        assert!(request.ports.is_empty());
        assert!(request.name.is_none());
        assert!(!request.auto_remove);
    }

    #[test]
    fn ports_and_volumes_take_an_address_and_a_mode() {
        let ports = parse_ports("8080:80, 0.0.0.0:8443:443\n127.0.0.1:2222:22")
            .unwrap_or_else(|reason| panic!("{reason}"));
        assert_eq!(
            ports.iter().map(ToString::to_string).collect::<Vec<_>>(),
            ["8080:80", "0.0.0.0:8443:443", "127.0.0.1:2222:22"]
        );
        let directory = std::env::temp_dir().display().to_string();
        let shares = parse_shares(&format!("{directory}:/mnt/a:ro\n{directory}:/mnt/b\n"))
            .unwrap_or_else(|reason| panic!("{reason}"));
        assert_eq!(
            shares
                .iter()
                .map(|share| share.readonly)
                .collect::<Vec<_>>(),
            [true, false]
        );
    }

    #[test]
    fn the_settings_show_addresses_and_modes_as_they_are_given() {
        let mut held = machine();
        held.ports.push(Port {
            address: Some(std::net::Ipv4Addr::UNSPECIFIED),
            host: 8443,
            guest: 443,
        });
        let directory = std::env::temp_dir();
        held.shares.push(Share {
            tag: "share".to_owned(),
            source: directory.clone(),
            target: "/mnt/a".to_owned(),
            readonly: true,
            pid: None,
            started: None,
        });
        let fields = SettingsFields::of(&held);
        assert_eq!(fields.ports, "8080:80, 0.0.0.0:8443:443");
        assert_eq!(fields.volumes, format!("{}:/mnt/a:ro", directory.display()));
        let changes = start_changes(&held, &fields).unwrap_or_else(|reason| panic!("{reason}"));
        assert_eq!(changes.ports, None);
        assert_eq!(changes.shares, None);
        let writable = SettingsFields {
            volumes: format!("{}:/mnt/a", directory.display()),
            ..fields
        };
        let changes = start_changes(&held, &writable).unwrap_or_else(|reason| panic!("{reason}"));
        assert!(
            changes
                .shares
                .is_some_and(|shares| shares.len() == 1 && !shares[0].readonly),
            "a change of mode is a change"
        );
    }

    #[test]
    fn a_run_request_reads_every_field() {
        let request = run_request(&RunFields {
            reference: "debian:trixie".to_owned(),
            name: "demo".to_owned(),
            memory: "4G".to_owned(),
            cpus: 3,
            ports: "8080:80, 2222:22".to_owned(),
            user: "dave".to_owned(),
            volumes: String::new(),
            disk_size: "40G".to_owned(),
            pull: 2,
            firmware: 2,
            cpu_model: "max".to_owned(),
            machine: 1,
            disk: 2,
            password: String::new(),
            ssh_config: 2,
            auto_remove: true,
        })
        .unwrap_or_else(|reason| panic!("{reason}"));
        assert!(request.auto_remove);
        assert_eq!(request.memory, 4096);
        assert_eq!(request.ports.len(), 2);
        assert_eq!(request.pull, Some(Pull::Always));
        assert_eq!(request.firmware, Some(Firmware::Uefi));
        assert_eq!(request.machine, Some(Chipset::Q35));
        assert_eq!(request.disk, Some(Disk::Sata));
        assert_eq!(request.ssh_config, Some(false));
        assert_eq!(request.disk_size.as_deref(), Some("40G"));
    }

    #[test]
    fn a_command_runs_through_sh_in_a_terminal_with_its_settings() {
        let request = exec_request(&ExecFields {
            command: "  ls -la | wc -l  ".to_owned(),
            workdir: " /srv ".to_owned(),
            env: "A=1\n\n  B=two words \n".to_owned(),
            root: true,
        })
        .unwrap_or_else(|reason| panic!("{reason}"));
        assert_eq!(request.command, ["sh", "-c", "ls -la | wc -l"]);
        assert!(request.interactive && request.tty && request.root);
        assert_eq!(request.workdir.as_deref(), Some("/srv"));
        assert_eq!(
            request.env,
            [
                ("A".to_owned(), "1".to_owned()),
                ("B".to_owned(), "two words".to_owned())
            ]
        );
        let plain = exec_request(&ExecFields {
            command: "uptime".to_owned(),
            ..ExecFields::default()
        })
        .unwrap_or_else(|reason| panic!("{reason}"));
        assert!(plain.env.is_empty() && plain.workdir.is_none() && !plain.root);
    }

    #[test]
    fn a_command_needs_a_command_line_and_well_formed_variables() {
        assert!(exec_request(&ExecFields::default()).is_err());
        assert!(
            exec_request(&ExecFields {
                command: "true".to_owned(),
                env: "NOVALUE".to_owned(),
                ..ExecFields::default()
            })
            .is_err()
        );
    }

    #[test]
    fn a_run_request_without_an_image_is_refused() {
        assert!(run_request(&RunFields::default()).is_err());
        assert!(
            run_request(&RunFields {
                reference: "x".to_owned(),
                ports: "eighty".to_owned(),
                ..RunFields::default()
            })
            .is_err()
        );
    }

    fn machine() -> Instance {
        Instance {
            name: "demo".to_owned(),
            image: "debian:trixie".to_owned(),
            digest: String::new(),
            arch: "amd64".to_owned(),
            created: 0,
            memory: 2048,
            cpus: 2,
            firmware: Firmware::Bios,
            cpu_model: "max".to_owned(),
            machine: Chipset::Q35,
            disk: Disk::Virtio,
            user: "dave".to_owned(),
            seeded: true,
            monitor: PathBuf::new(),
            ssh_port: None,
            pid: None,
            started: None,
            generation: 0,
            ssh_config: false,
            auto_remove: false,
            media: vm_core::catalogue::Media::Disk,
            cdrom: None,
            password: None,
            ports: vec![Port {
                address: None,
                host: 8080,
                guest: 80,
            }],
            shares: Vec::new(),
        }
    }

    #[test]
    fn the_settings_as_opened_are_no_change() {
        let held = machine();
        let fields = SettingsFields::of(&held);
        assert_eq!(fields.ports, "8080:80");
        assert_eq!(fields.memory, "2048M");
        let changes = start_changes(&held, &fields).unwrap_or_else(|reason| panic!("{reason}"));
        assert!(!changes.any());
    }

    #[test]
    fn only_what_differs_becomes_a_change() {
        let held = machine();
        let fields = SettingsFields {
            memory: "4G".to_owned(),
            ports: String::new(),
            ssh_config: true,
            remove_password: true,
            ..SettingsFields::of(&held)
        };
        let changes = start_changes(&held, &fields).unwrap_or_else(|reason| panic!("{reason}"));
        assert_eq!(changes.memory, Some(4096));
        assert_eq!(changes.ports, Some(Vec::new()));
        assert_eq!(changes.cpus, None);
        assert_eq!(changes.user, None);
        assert_eq!(changes.ssh_config, Some(true));
        assert_eq!(changes.password.as_deref(), Some("*"));
        assert_eq!(changes.firmware, None);
    }

    #[test]
    fn an_import_needs_a_source_and_a_name() {
        assert!(import_request(&ImportFields::default()).is_err());
        let fields = ImportFields {
            source: "https://example.com/x.iso".to_owned(),
            reference: "x:1".to_owned(),
            login: 0,
            forget_url: true,
            ..ImportFields::default()
        };
        let import = import_request(&fields).unwrap_or_else(|reason| panic!("{reason}"));
        assert_eq!(import.login, Login::CloudInit);
        assert!(!import.fetchable);
        assert_eq!(import.arch, vm_core::ARCHITECTURES[0]);
    }

    #[test]
    fn an_export_needs_a_file() {
        assert!(export_request("x:1", None, "amd64", 0, false).is_err());
        let file = PathBuf::from("/tmp/x");
        let export = export_request("x:1", Some(&file), "amd64", 3, true)
            .unwrap_or_else(|reason| panic!("{reason}"));
        assert_eq!(export.compression, Compression::Zstd);
        assert!(export.force);
    }

    #[test]
    fn a_copy_names_the_guest_side() {
        let host = PathBuf::from("/home/dave/file");
        let Ok(Command::Copy { from, to }) = copy_command("demo", true, Some(&host), "/tmp/file")
        else {
            panic!("not a copy");
        };
        assert_eq!(from, "/home/dave/file");
        assert_eq!(to, "demo:/tmp/file");
        let Ok(Command::Copy { from, .. }) = copy_command("demo", false, Some(&host), "/tmp/file")
        else {
            panic!("not a copy");
        };
        assert_eq!(from, "demo:/tmp/file");
        assert!(copy_command("demo", true, None, "/tmp").is_err());
    }

    #[test]
    fn ports_and_shares_are_read_from_loose_text() {
        let ports = parse_ports("80:80,\n 443:443 22:2222").unwrap_or_default();
        assert_eq!(ports.len(), 3);
        assert!(parse_ports("nope").is_err());
        assert!(parse_shares("").unwrap_or_default().is_empty());
        assert!(parse_shares("/nonexistent:/mnt").is_err());
    }
}
