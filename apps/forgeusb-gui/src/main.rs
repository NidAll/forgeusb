//! Unprivileged desktop shell around the shared planning and execution engine.
//! Blocking I/O always runs on workers; the event loop only renders and polls.
use forgeusb_core::{Cancellation, Progress, Stage};
use forgeusb_device::Device;
use forgeusb_image::checksums::{Checksums, PublishedSum};
use forgeusb_image::{Coverage, ImageInfo, ImageKind};
use forgeusb_linux::{HelperEvent, Plan, TargetKind, WriteRequest};
use forgeusb_windows::unattend::Customization;
use iced::widget::{
    button, checkbox, column, container, horizontal_rule, pick_list, progress_bar, row, scrollable,
    text, text_input,
};
use iced::{Color, Element, Fill, Font, Subscription, Task, Theme};
use std::collections::VecDeque;
use std::fmt;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::time::{Duration, Instant};

const HELPER: &str = "/usr/lib/forgeusb/forgeusb-helper";
const MAX_LOG_LINES: usize = 160;
/// Disposable-file images in Format mode have no device to size them by.
const TEST_FORMAT_BYTES: u64 = 512 * 1024 * 1024;

fn main() -> iced::Result {
    // This is a Linux desktop application. Only the narrow helper may be root.
    if unsafe { libc::geteuid() } == 0 {
        eprintln!(
            "ForgeUSB refuses to run its GUI as root. Launch it as your desktop user; physical writes use Polkit."
        );
        std::process::exit(2);
    }
    let options = Options::parse();
    iced::application(
        concat!("ForgeUSB ", env!("CARGO_PKG_VERSION")),
        State::update,
        State::view,
    )
    .theme(State::theme)
    .subscription(State::subscription)
    .settings(iced::Settings {
        default_text_size: 12.0.into(),
        ..Default::default()
    })
    .default_font(Font {
        family: iced::font::Family::Name("Noto Sans"),
        ..Font::DEFAULT
    })
    .window(iced::window::Settings {
        size: (520.0, 740.0).into(),
        min_size: Some((440.0, 560.0).into()),
        icon: Some(
            iced::window::icon::from_rgba(
                include_bytes!("../assets/forgeusb-64.rgba").to_vec(),
                64,
                64,
            )
            .expect("checked-in 64x64 RGBA icon"),
        ),
        platform_specific: iced::window::settings::PlatformSpecific {
            application_id: "forgeusb".into(),
            ..Default::default()
        },
        ..Default::default()
    })
    .exit_on_close_request(false)
    .run_with(move || (State::new(options), Task::none()))
}

#[derive(Default)]
struct Options {
    image: String,
    test_output: String,
    smoke: bool,
    mode: Option<(MediaMode, SchemeChoice)>,
}

impl Options {
    fn parse() -> Self {
        let mut result = Self::default();
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--image" => result.image = args.next().unwrap_or_default(),
                "--test-output" => result.test_output = args.next().unwrap_or_default(),
                "--smoke-test" => result.smoke = true,
                "--mode" => {
                    result.mode = Some(match args.next().as_deref() {
                        Some("raw") => (MediaMode::Raw, SchemeChoice::Mbr),
                        Some("windows-mbr") => (MediaMode::Windows, SchemeChoice::Mbr),
                        Some("windows-gpt") => (MediaMode::Windows, SchemeChoice::Gpt),
                        Some("format") => (MediaMode::Format, SchemeChoice::Mbr),
                        _ => {
                            eprintln!("--mode requires raw, windows-mbr, windows-gpt or format");
                            std::process::exit(2);
                        }
                    });
                }
                "--help" | "-h" => {
                    println!(
                        "ForgeUSB desktop\n\n  --image PATH          Prefill the source path\n  --mode MODE           raw, windows-mbr, windows-gpt or format\n  --test-output PATH    Prefill a NEW disposable output file\n  --smoke-test          Open for ten seconds without starting an operation\n\nThe GUI must run as your normal desktop user."
                    );
                    std::process::exit(0);
                }
                _ => {
                    eprintln!("Unknown argument: {arg}. Try --help.");
                    std::process::exit(2);
                }
            }
        }
        result
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DeviceChoice {
    path: PathBuf,
    label: String,
}

impl fmt::Display for DeviceChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.label)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WindowsOption {
    Requirements,
    OnlineAccount,
    LocalAccount,
    Regional,
    DataCollection,
    BitLocker,
}

#[derive(Debug, Clone)]
enum Message {
    Tick,
    Refresh,
    MediaMode(MediaMode),
    Scheme(SchemeChoice),
    Filesystem(FsChoice),
    Label(String),
    WindowsOption(WindowsOption, bool),
    AccountName(String),
    BrowseImage,
    Inspect,
    FileDropped(PathBuf),
    ChooseDevice(DeviceChoice),
    Unmount,
    EjectWhenDone(bool),
    TestMode(bool),
    TestPath(String),
    EditTestPath,
    TestPathDone,
    MakePlan,
    Confirmation(String),
    Start,
    StartRequested,
    DismissPlan,
    Cancel,
    ToggleTheme,
    ToggleLog,
    ToggleAdvanced,
    ToggleAdvancedFormat,
    ShowChecksums,
    ExpectedChecksum(String),
    CloseChecksums,
    FocusImage,
    FocusNext(bool),
    SelectRelative(i8),
    ToggleTestMode,
    CloseRequested,
}

enum WorkerEvent {
    PickedImage(Result<Option<String>, String>),
    Devices(Result<Vec<Device>, String>),
    Inspected(Result<ImageInfo, String>),
    Prepared(Result<PreparedMedia, String>),
    Planned(Result<Box<Plan>, String>),
    Progress(Progress),
    Finished(Result<String, String>),
    Unmounted(Result<Device, String>),
    Ejected(Result<String, String>),
    Checksums(Result<(Checksums, Option<PublishedSum>), String>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MediaMode {
    Raw,
    Windows,
    Format,
}
impl fmt::Display for MediaMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Raw => "Disk or ISO image (write as-is)",
            Self::Windows => "Windows installer (UEFI)",
            Self::Format => "Format only (non-bootable drive)",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
enum SchemeChoice {
    #[default]
    Mbr,
    Gpt,
}
impl fmt::Display for SchemeChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Mbr => "MBR",
            Self::Gpt => "GPT",
        })
    }
}
impl From<SchemeChoice> for forgeusb_partition::Scheme {
    fn from(value: SchemeChoice) -> Self {
        match value {
            SchemeChoice::Mbr => Self::Mbr,
            SchemeChoice::Gpt => Self::Gpt,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
enum FsChoice {
    #[default]
    Fat32,
    Exfat,
    Ntfs,
    Ext4,
}
impl FsChoice {
    const ALL: [Self; 4] = [Self::Fat32, Self::Exfat, Self::Ntfs, Self::Ext4];
    fn filesystem(self) -> forgeusb_filesystem::Filesystem {
        match self {
            Self::Fat32 => forgeusb_filesystem::Filesystem::Fat32,
            Self::Exfat => forgeusb_filesystem::Filesystem::Exfat,
            Self::Ntfs => forgeusb_filesystem::Filesystem::Ntfs,
            Self::Ext4 => forgeusb_filesystem::Filesystem::Ext4,
        }
    }
}
impl fmt::Display for FsChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Fat32 => "FAT32 (any computer, files up to 4 GB)",
            Self::Exfat => "exFAT (large files, Windows/macOS/Linux)",
            Self::Ntfs => "NTFS (Windows)",
            Self::Ext4 => "ext4 (Linux)",
        })
    }
}

/// Preferences remembered between sessions in $XDG_CONFIG_HOME/forgeusb.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct Settings {
    #[serde(default)]
    dark: bool,
    #[serde(default = "yes")]
    eject_when_done: bool,
    #[serde(default)]
    scheme: SchemeChoice,
    #[serde(default)]
    filesystem: FsChoice,
    #[serde(default)]
    windows: Customization,
    #[serde(default)]
    last_directory: Option<PathBuf>,
}
fn yes() -> bool {
    true
}
fn settings_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    base.is_absolute()
        .then(|| base.join("forgeusb").join("settings.json"))
}
fn load_settings() -> Settings {
    settings_path()
        .and_then(|p| std::fs::read(p).ok())
        .filter(|b| b.len() <= 64 * 1024)
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Settings {
            eject_when_done: true,
            ..Default::default()
        })
}

struct PreparedMedia {
    // Own the private directory until the final write/verification has finished.
    _directory: tempfile::TempDir,
    image: ImageInfo,
    /// Device capacity the image was sized for (None for disposable files).
    sized_for: Option<u64>,
    summary: String,
}

struct Running {
    cancel: Cancellation,
    progress: Progress,
    started: Instant,
    /// Start of the current stage, for throughput and time-remaining estimates.
    stage_started: Instant,
}

#[derive(Default)]
struct ChecksumView {
    computing: bool,
    result: Option<Checksums>,
    published: Option<PublishedSum>,
    expected: String,
    error: Option<String>,
}

struct State {
    image_path: String,
    image: Option<ImageInfo>,
    media_mode: MediaMode,
    scheme: SchemeChoice,
    filesystem: FsChoice,
    label: String,
    windows: Customization,
    account_name: String,
    host_locale: Option<String>,
    prepared: Option<PreparedMedia>,
    devices: Vec<Device>,
    choices: Vec<DeviceChoice>,
    selected: Option<DeviceChoice>,
    test_mode: bool,
    test_path: String,
    editing_test_path: bool,
    plan: Option<Plan>,
    confirmation: String,
    running: Option<Running>,
    writing_device: Option<Device>,
    eject_when_done: bool,
    inspecting: bool,
    planning: bool,
    discovering: bool,
    unmounting: bool,
    dark: bool,
    show_log: bool,
    advanced: bool,
    advanced_format: bool,
    confirm_open: bool,
    checksums: Option<ChecksumView>,
    picking: bool,
    picker_cancel: Cancellation,
    error: Option<String>,
    discovery_error: Option<String>,
    status: String,
    done: bool,
    log: VecDeque<String>,
    tx: SyncSender<WorkerEvent>,
    rx: Receiver<WorkerEvent>,
    ticks: u64,
    smoke_started: Option<Instant>,
    exit_after_operation: bool,
    focus_preview: bool,
    persist: bool,
}

impl State {
    fn new(options: Options) -> Self {
        let mut state = Self::empty(options);
        let settings = load_settings();
        state.dark = settings.dark;
        state.eject_when_done = settings.eject_when_done;
        state.filesystem = settings.filesystem;
        if state.media_mode == MediaMode::Raw {
            state.scheme = settings.scheme;
        }
        state.account_name = settings.windows.local_account.clone().unwrap_or_default();
        state.windows = Customization {
            regional: settings
                .windows
                .regional
                .as_ref()
                .and(state.host_locale.clone()),
            ..settings.windows
        };
        state.persist = true;
        state.append_log("ForgeUSB started as an unprivileged desktop user.");
        state.refresh();
        if !state.image_path.trim().is_empty() {
            let _ = state.update(Message::Inspect);
        }
        state
    }

    fn empty(options: Options) -> Self {
        let (tx, rx) = mpsc::sync_channel(256);
        let (media_mode, scheme) = options.mode.unwrap_or((MediaMode::Raw, SchemeChoice::Mbr));
        Self {
            image_path: options.image,
            image: None,
            media_mode,
            scheme,
            filesystem: FsChoice::Fat32,
            label: "FORGEUSB".into(),
            windows: Customization::default(),
            account_name: String::new(),
            host_locale: forgeusb_windows::unattend::host_locale(),
            prepared: None,
            devices: Vec::new(),
            choices: Vec::new(),
            selected: None,
            test_mode: !options.test_output.is_empty(),
            editing_test_path: options.test_output.is_empty(),
            test_path: options.test_output,
            plan: None,
            confirmation: String::new(),
            running: None,
            writing_device: None,
            eject_when_done: true,
            inspecting: false,
            planning: false,
            discovering: false,
            unmounting: false,
            dark: false,
            show_log: false,
            advanced: false,
            advanced_format: false,
            confirm_open: false,
            checksums: None,
            picking: false,
            picker_cancel: Cancellation::default(),
            error: None,
            discovery_error: None,
            status: "Ready.".into(),
            done: false,
            log: VecDeque::new(),
            tx,
            rx,
            ticks: 0,
            smoke_started: options.smoke.then(Instant::now),
            exit_after_operation: false,
            focus_preview: false,
            persist: false,
        }
    }

    fn save_settings(&self) {
        if !self.persist {
            return;
        }
        let settings = Settings {
            dark: self.dark,
            eject_when_done: self.eject_when_done,
            scheme: self.scheme,
            filesystem: self.filesystem,
            windows: self.customization(),
            last_directory: PathBuf::from(self.image_path.trim())
                .parent()
                .filter(|p| p.is_absolute())
                .map(PathBuf::from),
        };
        if let Some(path) = settings_path()
            && let Some(parent) = path.parent()
            && std::fs::create_dir_all(parent).is_ok()
            && let Ok(json) = serde_json::to_vec_pretty(&settings)
        {
            let _ = std::fs::write(path, json);
        }
    }

    /// Keep the volume label within the target file system's rules as it is typed.
    fn fit_label(&self, value: &str) -> String {
        let filesystem = match self.media_mode {
            MediaMode::Windows => FsChoice::Fat32,
            _ => self.filesystem,
        };
        match filesystem {
            FsChoice::Fat32 => forgeusb_media::fat_label(value),
            FsChoice::Exfat => value.chars().take(11).collect(),
            FsChoice::Ntfs => value.chars().take(32).collect(),
            FsChoice::Ext4 => {
                let mut label = String::new();
                for c in value.chars() {
                    if label.len() + c.len_utf8() > 16 {
                        break;
                    }
                    label.push(c);
                }
                label
            }
        }
    }

    /// Windows options as they will be applied, including the account name.
    fn customization(&self) -> Customization {
        let mut c = self.windows.clone();
        if c.local_account.is_some() {
            let name = self.account_name.trim();
            c.local_account = (!name.is_empty()).then(|| name.to_owned());
        }
        c
    }

    fn append_log(&mut self, line: impl Into<String>) {
        self.log.push_back(line.into());
        while self.log.len() > MAX_LOG_LINES {
            self.log.pop_front();
        }
    }

    fn invalidate_plan(&mut self) {
        self.clear_review();
        self.error = None;
        self.done = false;
    }

    fn clear_review(&mut self) {
        self.plan = None;
        self.confirm_open = false;
        self.focus_preview = false;
        self.confirmation.clear();
    }

    /// Prepared media depends on the source, options and target size.
    fn invalidate_prepared(&mut self) {
        self.prepared = None;
        self.invalidate_plan();
    }

    fn refresh(&mut self) {
        if self.discovering {
            return;
        }
        self.discovering = true;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = forgeusb_device::discover().map_err(|e| format!("{e:#}"));
            let _ = tx.send(WorkerEvent::Devices(result));
        });
    }

    fn editable(&self) -> bool {
        self.idle() && !self.confirm_open && !self.show_log && self.checksums.is_none()
    }

    fn selected_device(&self) -> Option<&Device> {
        let selected = self.selected.as_ref()?;
        self.devices.iter().find(|d| d.path == selected.path)
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Tick => {
                let events: Vec<_> = self.rx.try_iter().collect();
                for event in events {
                    self.worker_event(event);
                }
                if self.focus_preview {
                    self.focus_preview = false;
                    return text_input::focus(text_input::Id::new("confirmation"));
                }
                self.ticks = self.ticks.wrapping_add(1);
                if self.ticks.is_multiple_of(30) {
                    self.refresh();
                }
                if self.exit_after_operation && self.running.is_none() && !self.picking {
                    return iced::exit();
                }
                if self
                    .smoke_started
                    .is_some_and(|start| start.elapsed() >= Duration::from_secs(10))
                    && !self.exit_after_operation
                {
                    println!(
                        "ForgeUSB GUI smoke state: {}",
                        serde_json::json!({
                            "image": self.image.as_ref().map(|image| &image.path),
                            "plan_ready": self.plan.is_some(),
                            "confirmation_open": self.confirm_open,
                            "running": self.running.is_some(),
                        })
                    );
                    println!(
                        "ForgeUSB GUI smoke test: event loop ran, {} storage devices discovered; no write requested.",
                        self.devices.len()
                    );
                    return self.update(Message::CloseRequested);
                }
            }
            Message::Refresh => self.refresh(),
            Message::FileDropped(path) if self.editable() => {
                if let Some(path) = path.to_str().filter(|_| path.is_absolute()) {
                    self.image_path = path.to_owned();
                    self.image = None;
                    self.invalidate_prepared();
                    if self.media_mode == MediaMode::Format {
                        self.media_mode = MediaMode::Raw;
                    }
                    return self.update(Message::Inspect);
                }
            }
            Message::MediaMode(value) if self.editable() => {
                self.media_mode = value;
                self.invalidate_prepared();
                self.label = self.fit_label(&self.label.clone());
                self.status = match value {
                    MediaMode::Raw => "Choose a disk or ISO image, then press START.",
                    MediaMode::Windows => {
                        "Choose a Windows ISO. START builds verified installer media, then asks you to confirm."
                    }
                    MediaMode::Format => "Choose a file system, then press START to erase and format the drive.",
                }
                .into();
            }
            Message::Scheme(value) if self.editable() => {
                self.scheme = value;
                self.invalidate_prepared();
                self.save_settings();
            }
            Message::Filesystem(value) if self.editable() => {
                self.filesystem = value;
                self.label = self.fit_label(&self.label.clone());
                self.invalidate_prepared();
                self.save_settings();
            }
            Message::Label(value) if self.editable() => {
                self.label = self.fit_label(&value);
                self.invalidate_prepared();
            }
            Message::WindowsOption(option, on) if self.editable() => {
                match option {
                    WindowsOption::Requirements => self.windows.bypass_requirements = on,
                    WindowsOption::OnlineAccount => self.windows.bypass_online_account = on,
                    WindowsOption::LocalAccount => {
                        self.windows.local_account = on.then(|| self.account_name.clone());
                    }
                    WindowsOption::Regional => {
                        self.windows.regional = if on { self.host_locale.clone() } else { None };
                    }
                    WindowsOption::DataCollection => self.windows.disable_data_collection = on,
                    WindowsOption::BitLocker => self.windows.disable_bitlocker = on,
                }
                self.invalidate_prepared();
                self.save_settings();
            }
            Message::AccountName(value) if self.editable() => {
                self.account_name = value.chars().take(20).collect();
                self.invalidate_prepared();
            }
            Message::BrowseImage if self.editable() => {
                self.invalidate_plan();
                self.picking = true;
                self.picker_cancel = Cancellation::default();
                self.status = "Select an image.".into();
                let cancel = self.picker_cancel.clone();
                let tx = self.tx.clone();
                std::thread::spawn(move || {
                    let _ = tx.send(WorkerEvent::PickedImage(select_image(&cancel)));
                });
            }
            Message::Inspect if self.editable() && !self.image_path.trim().is_empty() => {
                self.inspecting = true;
                self.invalidate_plan();
                self.status = "Analyzing image structure and calculating SHA-256…".into();
                let path = PathBuf::from(self.image_path.trim());
                let tx = self.tx.clone();
                std::thread::spawn(move || {
                    let result = forgeusb_image::inspect(&path).map_err(|e| format!("{e:#}"));
                    let _ = tx.send(WorkerEvent::Inspected(result));
                });
            }
            Message::ChooseDevice(value) if self.editable() => {
                if self.selected.as_ref().map(|s| &s.path) != Some(&value.path) {
                    self.prepared = None;
                }
                self.selected = Some(value);
                self.invalidate_plan();
            }
            Message::Unmount if self.editable() && !self.unmounting => {
                if let Some(device) = self.selected_device() {
                    let path = device.path.clone();
                    self.unmounting = true;
                    self.status = "Unmounting the drive…".into();
                    let tx = self.tx.clone();
                    std::thread::spawn(move || {
                        let result =
                            forgeusb_linux::session::unmount(&path).map_err(|e| format!("{e:#}"));
                        let _ = tx.send(WorkerEvent::Unmounted(result));
                    });
                }
            }
            Message::EjectWhenDone(value) => {
                self.eject_when_done = value;
                self.save_settings();
            }
            Message::TestMode(value) if self.editable() => {
                self.test_mode = value;
                self.selected = None;
                self.invalidate_prepared();
            }
            Message::TestPath(value) if self.editable() => {
                self.test_path = value;
                self.invalidate_plan();
            }
            Message::EditTestPath if self.editable() => {
                self.editing_test_path = true;
                return text_input::focus(text_input::Id::new("test-output"));
            }
            Message::TestPathDone => {
                self.editing_test_path = self.test_path.trim().is_empty();
            }
            Message::MakePlan if self.can_plan() => {
                self.planning = true;
                self.invalidate_plan();
                let capacity = (!self.test_mode)
                    .then(|| self.selected_device().map(|d| d.capacity))
                    .flatten();
                if self
                    .prepared
                    .as_ref()
                    .is_some_and(|p| p.sized_for != capacity)
                {
                    // Media built for another drive size must be rebuilt.
                    self.prepared = None;
                }
                if self.needs_preparation() && self.prepared.is_none() {
                    self.prepare();
                    return Task::none();
                }
                self.status = "Building a read-only operation plan…".into();
                let (source, coverage) = match &self.prepared {
                    Some(media) => (media.image.path.clone(), media.image.coverage),
                    None => (PathBuf::from(self.image_path.trim()), Coverage::Full),
                };
                let target = self.target().expect("can_plan requires a target");
                let test_file = self.test_mode;
                let tx = self.tx.clone();
                std::thread::spawn(move || {
                    let result = forgeusb_linux::plan_with(
                        &source,
                        &target,
                        test_file,
                        coverage,
                        &Cancellation::default(),
                    )
                    .map(Box::new)
                    .map_err(|e| format!("{e:#}"));
                    let _ = tx.send(WorkerEvent::Planned(result));
                });
            }
            Message::Confirmation(value) if self.running.is_none() => self.confirmation = value,
            Message::StartRequested => {
                return self.update(if self.confirm_open {
                    Message::Start
                } else {
                    Message::MakePlan
                });
            }
            Message::DismissPlan if self.running.is_none() => {
                self.invalidate_plan();
                self.status = "Cancelled before writing. Nothing was changed.".into();
            }
            Message::Start if self.can_start() => self.start(),
            Message::Cancel => {
                if let Some(running) = &self.running {
                    running.cancel.cancel();
                    self.status = "Cancelling at the next safe point…".into();
                    self.append_log(if self.planning {
                        "Preparation cancellation requested; no USB write has started."
                    } else {
                        "Cancellation requested; the target may contain an incomplete image."
                    });
                } else if self.picking {
                    self.picker_cancel.cancel();
                } else if self.checksums.is_some() {
                    self.checksums = None;
                } else if self.confirm_open {
                    return self.update(Message::DismissPlan);
                } else if self.show_log {
                    self.show_log = false;
                }
            }
            Message::ToggleTheme => {
                self.dark = !self.dark;
                self.save_settings();
            }
            Message::ToggleLog if !self.confirm_open => self.show_log = !self.show_log,
            Message::ToggleAdvanced => self.advanced = !self.advanced,
            Message::ToggleAdvancedFormat => self.advanced_format = !self.advanced_format,
            Message::ShowChecksums if self.editable() && !self.image_path.trim().is_empty() => {
                let path = PathBuf::from(self.image_path.trim());
                self.checksums = Some(ChecksumView {
                    computing: true,
                    ..Default::default()
                });
                let tx = self.tx.clone();
                std::thread::spawn(move || {
                    let result =
                        forgeusb_image::checksums::compute(&path, &Cancellation::default(), |_| {})
                            .map(|sums| (sums, forgeusb_image::checksums::find_published(&path)))
                            .map_err(|e| format!("{e:#}"));
                    let _ = tx.send(WorkerEvent::Checksums(result));
                });
            }
            Message::ExpectedChecksum(value) => {
                if let Some(view) = &mut self.checksums {
                    view.expected = value.chars().take(160).collect();
                }
            }
            Message::CloseChecksums => self.checksums = None,
            Message::FocusImage => return self.update(Message::BrowseImage),
            Message::FocusNext(backward) => {
                return if backward {
                    iced::widget::focus_previous()
                } else {
                    iced::widget::focus_next()
                };
            }
            Message::SelectRelative(direction)
                if self.editable() && !self.test_mode && !self.choices.is_empty() =>
            {
                let current = self
                    .selected
                    .as_ref()
                    .and_then(|s| self.choices.iter().position(|d| d.path == s.path));
                let index = match current {
                    Some(index) if direction < 0 => {
                        (index + self.choices.len() - 1) % self.choices.len()
                    }
                    Some(index) => (index + 1) % self.choices.len(),
                    None if direction < 0 => self.choices.len() - 1,
                    None => 0,
                };
                return self.update(Message::ChooseDevice(self.choices[index].clone()));
            }
            Message::ToggleTestMode if self.editable() => {
                return self.update(Message::TestMode(!self.test_mode));
            }
            Message::CloseRequested => {
                if self.picking {
                    self.picker_cancel.cancel();
                    self.exit_after_operation = true;
                } else if let Some(running) = &self.running {
                    running.cancel.cancel();
                    self.exit_after_operation = true;
                    self.status =
                        "Finishing cancellation before closing. The target may be incomplete."
                            .into();
                } else {
                    return iced::exit();
                }
            }
            _ => {}
        }
        Task::none()
    }

    fn worker_event(&mut self, event: WorkerEvent) {
        match event {
            WorkerEvent::PickedImage(result) => {
                self.picking = false;
                if self.exit_after_operation {
                    return;
                }
                match result {
                    Ok(Some(path)) => {
                        self.image_path = path;
                        self.image = None;
                        self.prepared = None;
                        if self.media_mode == MediaMode::Format {
                            self.media_mode = MediaMode::Raw;
                        }
                        let _ = self.update(Message::Inspect);
                    }
                    Ok(None) => self.status = "Ready.".into(),
                    Err(error) => self.fail(error),
                }
            }
            WorkerEvent::Devices(result) => {
                self.discovering = false;
                match result {
                    Ok(devices) => {
                        self.discovery_error = None;
                        self.choices = devices
                            .iter()
                            .filter(|d| d.eligible || d.blocked_only_by_mounts)
                            .map(|d| DeviceChoice {
                                path: d.path.clone(),
                                label: format!(
                                    "{} · {} · {}{}",
                                    ellipsize(&device_model(d), 26),
                                    drive_size(d.capacity),
                                    d.path.display(),
                                    if d.eligible { "" } else { " (mounted)" }
                                ),
                            })
                            .collect();
                        if let Some(selected) = &self.selected {
                            let new = devices.iter().find(|d| d.path == selected.path);
                            let old = self.devices.iter().find(|d| d.path == selected.path);
                            let unchanged = new.zip(old).is_some_and(|(new, old)| {
                                (new.eligible || new.blocked_only_by_mounts)
                                    && new.identity == old.identity
                                    && new.capacity == old.capacity
                            });
                            if !unchanged && (self.running.is_none() || self.planning) {
                                self.selected = None;
                                self.prepared = None;
                                self.invalidate_plan();
                                self.status =
                                    "The selected drive changed or was disconnected. Choose it again."
                                        .into();
                                self.append_log("Target selection cleared because device eligibility or identity changed.");
                            } else if let Some(choice) =
                                self.choices.iter().find(|c| c.path == selected.path)
                            {
                                self.selected = Some(choice.clone());
                            }
                        }
                        self.devices = devices;
                    }
                    Err(error) => {
                        self.devices.clear();
                        self.choices.clear();
                        if (self.running.is_none() || self.planning) && !self.test_mode {
                            self.selected = None;
                            self.clear_review();
                        }
                        if self.discovery_error.as_ref() != Some(&error) {
                            self.append_log(format!("Device discovery failed: {error}"));
                        }
                        self.discovery_error = Some(error);
                    }
                }
            }
            WorkerEvent::Inspected(result) => {
                self.inspecting = false;
                match result {
                    Ok(image) => {
                        self.status = "Image analyzed. Press START to continue.".into();
                        if image.windows && !image.bootable_disk {
                            if self.media_mode != MediaMode::Windows {
                                self.media_mode = MediaMode::Windows;
                            }
                            self.status = "Windows installer detected. START builds verified installer media before asking you to confirm.".into();
                        } else if self.media_mode == MediaMode::Windows && !image.windows {
                            self.media_mode = MediaMode::Raw;
                        }
                        if image.kind == ImageKind::Compressed {
                            self.status = "Compressed image. START expands it in your cache, then asks you to confirm.".into();
                        }
                        self.append_log(format!(
                            "Analyzed {} ({}); SHA-256 {}",
                            image.path.display(),
                            size(image.size),
                            image.sha256
                        ));
                        self.image = Some(image);
                        self.save_settings();
                    }
                    Err(error) => self.fail(error),
                }
            }
            WorkerEvent::Prepared(result) => {
                let cancelled = self
                    .running
                    .as_ref()
                    .is_some_and(|running| running.cancel.is_cancelled());
                self.planning = false;
                self.running = None;
                if self.exit_after_operation {
                    return;
                }
                if cancelled {
                    self.status = "Preparation cancelled. No USB write has started.".into();
                    return;
                }
                match result {
                    Ok(prepared) => {
                        self.append_log(prepared.summary.clone());
                        if self.media_mode == MediaMode::Raw {
                            // A decompressed image replaces the compressed one for review.
                        } else {
                            self.image = Some(prepared.image.clone());
                        }
                        self.prepared = Some(prepared);
                        let _ = self.update(Message::MakePlan);
                    }
                    Err(error) => self.fail(error),
                }
            }
            WorkerEvent::Planned(result) => {
                self.planning = false;
                match result {
                    Ok(plan) => {
                        let current_target = self.target();
                        let same_device = match &plan.target_kind {
                            TargetKind::TestFile => self.test_mode,
                            TargetKind::Block(planned) => {
                                !self.test_mode
                                    && self.devices.iter().any(|device| {
                                        device.eligible
                                            && device.path == planned.path
                                            && device.identity == planned.identity
                                    })
                            }
                        };
                        if current_target.as_ref() != Some(&plan.target) || !same_device {
                            self.invalidate_plan();
                            self.status = "Target changed while planning. Select it again and create a new preview.".into();
                            return;
                        }
                        self.append_log(format!(
                            "Read-only preview: {} -> {} ({} bytes)",
                            plan.image.path.display(),
                            plan.target.display(),
                            plan.image.size
                        ));
                        self.status = "Check the drive and confirm.".into();
                        self.plan = Some(*plan);
                        self.confirm_open = true;
                        self.show_log = false;
                        self.focus_preview = true;
                    }
                    Err(error) => self.fail(error),
                }
            }
            WorkerEvent::Progress(progress) => {
                if let Some(running) = &mut self.running {
                    let changed = running.progress.stage != progress.stage;
                    if changed {
                        running.stage_started = Instant::now();
                    }
                    running.progress = progress.clone();
                    if changed {
                        self.append_log(format!("{:?}: {}", progress.stage, progress.message));
                    }
                    self.status = progress.message;
                }
            }
            WorkerEvent::Finished(result) => {
                self.running = None;
                self.confirm_open = false;
                self.plan = None;
                self.confirmation.clear();
                let device = self.writing_device.take();
                match result {
                    Ok(summary) => {
                        println!("ForgeUSB operation complete: {summary}");
                        self.done = true;
                        self.append_log(summary.clone());
                        self.status = summary;
                        if let Some(device) = device.filter(|_| self.eject_when_done) {
                            self.status = "Verified. Safely removing the drive…".into();
                            let tx = self.tx.clone();
                            std::thread::spawn(move || {
                                let result = forgeusb_linux::session::power_off(&device)
                                    .map(|()| format!("Verified and safely removed {}. You can unplug it now.", device_model(&device)))
                                    .map_err(|e| format!("{e:#}"));
                                let _ = tx.send(WorkerEvent::Ejected(result));
                            });
                        }
                    }
                    Err(error) => self.fail(error),
                }
                self.refresh();
            }
            WorkerEvent::Unmounted(result) => {
                self.unmounting = false;
                match result {
                    Ok(device) => {
                        self.append_log(format!("Unmounted {}.", device.path.display()));
                        self.status = "Drive unmounted. Press START to continue.".into();
                        self.error = None;
                    }
                    Err(error) => self.fail(format!(
                        "Could not unmount the drive: {error}. Close any open files on it, or eject it in your file manager, then press Refresh."
                    )),
                }
                self.refresh();
            }
            WorkerEvent::Ejected(result) => {
                match result {
                    Ok(message) => {
                        self.append_log(message.clone());
                        self.status = message;
                    }
                    Err(error) => {
                        self.append_log(format!("Safe removal failed: {error}"));
                        self.status = "Verified. Eject the drive from your file manager before unplugging it.".into();
                    }
                }
            }
            WorkerEvent::Checksums(result) => {
                if let Some(view) = &mut self.checksums {
                    view.computing = false;
                    match result {
                        Ok((sums, published)) => {
                            if let Some(p) = &published {
                                view.expected = p.digest.clone();
                            }
                            view.result = Some(sums);
                            view.published = published;
                        }
                        Err(error) => view.error = Some(error),
                    }
                }
            }
        }
    }

    fn fail(&mut self, error: String) {
        self.status = "Stopped. See the message above.".into();
        self.append_log(format!("Error: {error}"));
        self.error = Some(error);
    }

    fn idle(&self) -> bool {
        self.running.is_none()
            && !self.inspecting
            && !self.planning
            && !self.picking
            && !self.unmounting
    }

    fn target(&self) -> Option<PathBuf> {
        if self.test_mode {
            (!self.test_path.trim().is_empty()).then(|| PathBuf::from(self.test_path.trim()))
        } else {
            self.selected.as_ref().map(|d| d.path.clone())
        }
    }

    fn needs_preparation(&self) -> bool {
        match self.media_mode {
            MediaMode::Raw => self
                .image
                .as_ref()
                .is_some_and(|image| image.kind == ImageKind::Compressed),
            MediaMode::Windows | MediaMode::Format => true,
        }
    }

    fn can_plan(&self) -> bool {
        let source_ready = match self.media_mode {
            MediaMode::Raw => {
                !self.image_path.trim().is_empty()
                    && self.image.as_ref().is_none_or(|image| {
                        image.bootable_disk || image.kind == ImageKind::Compressed
                    })
            }
            MediaMode::Windows => !self.image_path.trim().is_empty(),
            MediaMode::Format => true,
        };
        let target_ready = self.test_mode || self.selected_device().is_some_and(|d| d.eligible);
        self.editable()
            && source_ready
            && (!self.needs_preparation()
                || self.smoke_started.is_none()
                || self.prepared.is_some())
            && self.target().is_some()
            && target_ready
    }

    fn prepare(&mut self) {
        let source = PathBuf::from(self.image_path.trim());
        let mode = self.media_mode;
        let scheme = self.scheme.into();
        let filesystem = self.filesystem.filesystem();
        let label = self.fit_label(self.label.trim());
        let customization = self.customization();
        let capacity = if self.test_mode {
            None
        } else {
            self.selected_device().map(|d| d.capacity)
        };
        let cancel = Cancellation::default();
        let message = match mode {
            MediaMode::Windows => "Building Windows installer media in your cache…",
            MediaMode::Format => "Formatting a private image of the drive…",
            MediaMode::Raw => "Decompressing the image into your cache…",
        };
        self.running = Some(Running {
            cancel: cancel.clone(),
            progress: forgeusb_core::progress(Stage::Inspecting, 0, 0, message),
            started: Instant::now(),
            stage_started: Instant::now(),
        });
        self.status = format!("{message} Nothing is written to the drive yet.");
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<PreparedMedia> {
                // Real installer media can exceed a RAM-backed /tmp. Keep it
                // on the user's cache filesystem, with XDG_CACHE_HOME available
                // for explicitly chosen staging storage and isolated tests.
                let cache = std::env::var_os("XDG_CACHE_HOME")
                    .map(PathBuf::from)
                    .or_else(|| {
                        std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache"))
                    })
                    .ok_or_else(|| {
                        anyhow::anyhow!("HOME or XDG_CACHE_HOME is required for staging")
                    })?;
                anyhow::ensure!(cache.is_absolute(), "cache directory must be absolute");
                let cache = cache.join("forgeusb");
                std::fs::create_dir_all(&cache)?;
                let prefix = match mode {
                    MediaMode::Windows => "windows-",
                    MediaMode::Format => "format-",
                    MediaMode::Raw => "expand-",
                };
                let directory = tempfile::Builder::new().prefix(prefix).tempdir_in(cache)?;
                let output = directory.path().join("prepared.img");
                let emit = |progress| {
                    let _ = tx.try_send(WorkerEvent::Progress(progress));
                };
                let (coverage, summary, expected) = match mode {
                    MediaMode::Windows => {
                        let report = forgeusb_media::build_windows_image(
                            &source,
                            &output,
                            &forgeusb_media::BuildOptions {
                                scheme,
                                capacity_bytes: capacity,
                                timeout: Duration::from_secs(7200),
                                label,
                                customization,
                            },
                            &cancel,
                            emit,
                        )?;
                        let mut summary = format!(
                            "Built and verified Windows installer media: {} files, {} of data. Windows Setup and Secure Boot are not validated by this step.",
                            report.files_verified,
                            size(report.payload_bytes)
                        );
                        for c in &report.customizations {
                            summary += &format!("\nSetup option: {c}");
                        }
                        (Coverage::DataRanges, summary, Some((report.image_bytes, report.sha256)))
                    }
                    MediaMode::Format => {
                        let report = forgeusb_media::format_image(
                            &output,
                            &forgeusb_media::FormatOptions {
                                filesystem,
                                scheme,
                                capacity_bytes: capacity.unwrap_or(TEST_FORMAT_BYTES),
                                label,
                                timeout: Duration::from_secs(3600),
                            },
                            &cancel,
                            emit,
                        )?;
                        (
                            Coverage::DataRanges,
                            format!("Formatted and checked a {:?} image for the drive.", report.filesystem),
                            Some((report.image_bytes, report.sha256)),
                        )
                    }
                    MediaMode::Raw => {
                        let report = forgeusb_media::decompress::decompress_image(
                            &source,
                            &output,
                            capacity.unwrap_or(forgeusb_media::MAX_IMAGE_BYTES),
                            &cancel,
                            emit,
                        )?;
                        (
                            Coverage::Full,
                            format!(
                                "Expanded {} image to {} (compressed file SHA-256 {}).",
                                report.compression.name(),
                                size(report.bytes),
                                report.source_sha256
                            ),
                            None,
                        )
                    }
                };
                cancel.check()?;
                let image = forgeusb_image::inspect_with(&output, &cancel, coverage)?;
                if let Some((bytes, sha256)) = expected {
                    anyhow::ensure!(
                        image.size == bytes && image.sha256 == sha256,
                        "prepared image changed before review; prepare it again"
                    );
                }
                anyhow::ensure!(
                    image.bootable_disk,
                    "the expanded file is not a recognized disk image: {}",
                    image.warnings.join(" ")
                );
                Ok(PreparedMedia {
                    _directory: directory,
                    image,
                    sized_for: capacity,
                    summary,
                })
            })()
            .map_err(|error| format!("{error:#}"));
            let _ = tx.send(WorkerEvent::Prepared(result));
        });
    }

    /// What the user types to confirm: the drive's short name, or CREATE for a file.
    fn confirmation_token(plan: &Plan) -> String {
        match &plan.target_kind {
            TargetKind::Block(device) => device
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| plan.confirmation.clone()),
            TargetKind::TestFile => "CREATE".into(),
        }
    }

    fn can_start(&self) -> bool {
        self.idle()
            && self.confirm_open
            && !self.show_log
            && self.smoke_started.is_none()
            && self
                .plan
                .as_ref()
                .is_some_and(|p| self.confirmation.trim() == Self::confirmation_token(p))
    }

    fn start(&mut self) {
        let plan = self.plan.clone().expect("can_start requires a plan");
        // The typed token proves the user named this drive; the engine and
        // helper still require the plan's exact, identity-bound confirmation.
        let confirmation = plan.confirmation.clone();
        self.confirm_open = false;
        let cancel = Cancellation::default();
        self.running = Some(Running {
            cancel: cancel.clone(),
            progress: Progress {
                stage: Stage::Revalidating,
                completed: 0,
                total: plan.image.transfer_bytes(),
                message: "Revalidating source and target…".into(),
            },
            started: Instant::now(),
            stage_started: Instant::now(),
        });
        self.writing_device = match &plan.target_kind {
            TargetKind::Block(device) => Some((**device).clone()),
            TargetKind::TestFile => None,
        };
        self.error = None;
        self.done = false;
        self.append_log(
            "Write explicitly confirmed. Source and target will be revalidated before I/O.",
        );
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = match &plan.target_kind {
                TargetKind::TestFile => {
                    forgeusb_linux::execute(&plan, &confirmation, &cancel, |progress| {
                        let _ = tx.try_send(WorkerEvent::Progress(progress));
                    })
                    .and_then(|report| {
                        anyhow::ensure!(
                            report.verified,
                            "operation returned without read-back verification"
                        );
                        Ok(completion_summary(&report))
                    })
                    .map_err(|e| format!("{e:#}"))
                }
                TargetKind::Block(_) => helper_write(&plan, &confirmation, &cancel, &tx),
            };
            let _ = tx.send(WorkerEvent::Finished(result));
        });
    }

    fn theme(&self) -> Theme {
        Theme::custom(
            "ForgeUSB classic".into(),
            if self.dark {
                iced::theme::Palette {
                    background: Color::from_rgb8(38, 38, 38),
                    text: Color::from_rgb8(238, 238, 238),
                    primary: Color::from_rgb8(78, 151, 218),
                    success: Color::from_rgb8(6, 176, 37),
                    danger: Color::from_rgb8(235, 115, 110),
                }
            } else {
                iced::theme::Palette {
                    background: Color::from_rgb8(240, 240, 240),
                    text: Color::from_rgb8(0, 0, 0),
                    primary: Color::from_rgb8(0, 102, 204),
                    success: Color::from_rgb8(6, 140, 37),
                    danger: Color::from_rgb8(165, 28, 28),
                }
            },
        )
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            iced::time::every(Duration::from_millis(100)).map(|_| Message::Tick),
            iced::window::close_requests().map(|_| Message::CloseRequested),
            iced::event::listen_with(|event, _, _| match event {
                iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
                    key, modifiers, ..
                }) => shortcut(key, modifiers),
                iced::Event::Window(iced::window::Event::FileDropped(path)) => {
                    Some(Message::FileDropped(path))
                }
                _ => None,
            }),
        ])
    }

    /// One plain sentence telling the user what to do next.
    fn hint(&self) -> String {
        if self.done {
            return self.status.clone();
        }
        if !self.test_mode && self.selected.is_none() {
            return if self.choices.is_empty() {
                "Plug in a USB drive.".into()
            } else {
                "Choose the USB drive to use.".into()
            };
        }
        if self.selected_device().is_some_and(|d| !d.eligible) && !self.test_mode {
            return "The drive is in use. Press Unmount to continue.".into();
        }
        if self.media_mode != MediaMode::Format && self.image_path.trim().is_empty() {
            return "Choose an image with SELECT, or drop a file on this window.".into();
        }
        "Press START. You will review everything before anything is erased.".into()
    }

    fn view(&self) -> Element<'_, Message> {
        let editable = self.editable();
        let mut drive = column![section_title("Drive Properties")].spacing(6);
        let target: Element<'_, Message> = if self.test_mode {
            if self.editing_test_path && editable {
                text_input("New output file (must not exist)", &self.test_path)
                    .id(text_input::Id::new("test-output"))
                    .size(12)
                    .padding([2, 5])
                    .style(classic_input)
                    .on_input(Message::TestPath)
                    .on_submit(Message::TestPathDone)
                    .into()
            } else {
                path_box(
                    self.test_path.clone(),
                    "New output file (must not exist)",
                    editable.then_some(Message::EditTestPath),
                )
            }
        } else if editable {
            pick_list(
                self.choices.clone(),
                self.selected.clone(),
                Message::ChooseDevice,
            )
            .placeholder(if self.choices.is_empty() {
                "No USB drives found — plug one in"
            } else {
                "Choose a USB drive"
            })
            .text_size(12)
            .padding([2, 5])
            .width(Fill)
            .style(classic_pick_list)
            .into()
        } else {
            readonly(
                self.selected
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "No USB drive selected".into()),
            )
        };
        drive = drive.push(field(
            if self.test_mode {
                "Device (new image file)"
            } else {
                "Device"
            },
            target,
        ));
        if let Some(device) = self.selected_device().filter(|_| !self.test_mode) {
            let contents = device
                .partitions
                .iter()
                .map(|p| {
                    let name = p
                        .label
                        .clone()
                        .unwrap_or_else(|| p.filesystem.clone().unwrap_or_else(|| "data".into()));
                    format!("{name} ({})", size(p.capacity))
                })
                .collect::<Vec<_>>();
            let summary = if contents.is_empty() {
                "No partitions".to_owned()
            } else {
                format!("Contains: {}", contents.join(", "))
            };
            if device.eligible {
                drive = drive.push(text(summary).size(11).style(text::secondary));
            } else {
                drive = drive.push(
                    row![
                        text(format!(
                            "{summary}. It is mounted, so it can't be written yet."
                        ))
                        .size(11)
                        .width(Fill),
                        button(text("Unmount").size(12))
                            .padding([2, 10])
                            .style(classic_button)
                            .on_press_maybe(editable.then_some(Message::Unmount)),
                    ]
                    .spacing(8)
                    .align_y(iced::Center),
                );
            }
        }
        let can_browse = editable && self.media_mode != MediaMode::Format;
        let source = path_box(
            if self.media_mode == MediaMode::Format {
                String::new()
            } else {
                self.image_path.trim().to_owned()
            },
            if self.media_mode == MediaMode::Format {
                "Not needed to format a drive"
            } else {
                "Click SELECT, or drop an image file on this window"
            },
            can_browse.then_some(Message::BrowseImage),
        );
        drive = drive.push(field(
            "Boot selection",
            row![
                source,
                button(text("SELECT").size(12).center())
                    .width(96)
                    .padding([2, 8])
                    .style(classic_button)
                    .on_press_maybe(can_browse.then_some(Message::BrowseImage)),
            ]
            .spacing(8)
            .align_y(iced::Center),
        ));
        if let Some(image) = self
            .image
            .as_ref()
            .filter(|_| self.media_mode != MediaMode::Format)
        {
            let shown = if self.prepared.is_some() && self.media_mode == MediaMode::Windows {
                "Prepared Windows media".to_owned()
            } else {
                kind_name(image)
            };
            drive = drive.push(
                row![
                    text(format!(
                        "{shown} · {} · SHA-256 {}…",
                        size(image.size),
                        &image.sha256[..12.min(image.sha256.len())]
                    ))
                    .size(11)
                    .style(text::secondary)
                    .width(Fill),
                    button(text("Checksums…").size(11))
                        .padding([1, 6])
                        .style(classic_button)
                        .on_press_maybe(editable.then_some(Message::ShowChecksums)),
                ]
                .spacing(8)
                .align_y(iced::Center),
            );
        }
        drive = drive.push(field(
            "Image option",
            if editable {
                Element::from(
                    pick_list(
                        [MediaMode::Raw, MediaMode::Windows, MediaMode::Format],
                        Some(self.media_mode),
                        Message::MediaMode,
                    )
                    .text_size(12)
                    .padding([2, 5])
                    .width(Fill)
                    .style(classic_pick_list),
                )
            } else {
                readonly(self.media_mode.to_string())
            },
        ));
        let scheme: Element<'_, Message> = if self.media_mode == MediaMode::Raw {
            readonly("From image")
        } else if editable {
            pick_list(
                [SchemeChoice::Mbr, SchemeChoice::Gpt],
                Some(self.scheme),
                Message::Scheme,
            )
            .text_size(12)
            .padding([2, 5])
            .width(Fill)
            .style(classic_pick_list)
            .into()
        } else {
            readonly(self.scheme.to_string())
        };
        drive = drive.push(
            row![
                field("Partition scheme", scheme),
                field(
                    "Target system",
                    readonly(match self.media_mode {
                        MediaMode::Raw => "From image",
                        MediaMode::Windows => "UEFI",
                        MediaMode::Format => "None (data only)",
                    })
                ),
            ]
            .spacing(36),
        );
        drive = drive.push(
            button(
                text(if self.advanced {
                    "Hide advanced drive properties"
                } else {
                    "Show advanced drive properties"
                })
                .size(12),
            )
            .padding([1, 0])
            .style(button::text)
            .on_press_maybe(
                (!self.confirm_open && !self.show_log).then_some(Message::ToggleAdvanced),
            ),
        );
        if self.advanced {
            let file_mode = checkbox(
                "Use a disposable image file instead of a drive",
                self.test_mode,
            )
            .size(14)
            .text_size(12);
            drive = drive.push(if editable {
                file_mode.on_toggle(Message::TestMode)
            } else {
                file_mode
            });
            if let Some(image) = &self.image {
                drive = drive.push(
                    text(format!("SHA-256: {}", image.sha256))
                        .size(10)
                        .font(Font::MONOSPACE),
                );
                for warning in &image.warnings {
                    drive = drive.push(text(warning).size(11).style(text::secondary));
                }
            }
            if let Some(device) = self.selected_device() {
                drive = drive.push(
                    text(format!(
                        "{} {} — {} bytes, {}:{}, serial {}",
                        device.vendor,
                        device.model,
                        device.capacity,
                        device.identity.major,
                        device.identity.minor,
                        device.identity.serial.as_deref().unwrap_or("unavailable")
                    ))
                    .size(11),
                );
            }
            for device in self
                .devices
                .iter()
                .filter(|device| !device.eligible && !device.blocked_only_by_mounts)
            {
                drive = drive.push(
                    text(format!(
                        "Not offered: {} — {}",
                        device.path.display(),
                        device.reasons.join("; ")
                    ))
                    .size(11)
                    .style(text::secondary),
                );
            }
        }

        let label: Element<'_, Message> = if self.media_mode == MediaMode::Raw {
            readonly("From image")
        } else {
            let input = text_input("Volume label", &self.label)
                .size(12)
                .padding([2, 5])
                .style(classic_input);
            if editable {
                input.on_input(Message::Label).into()
            } else {
                input.into()
            }
        };
        let filesystem: Element<'_, Message> = match self.media_mode {
            MediaMode::Raw => readonly("From image"),
            MediaMode::Windows => readonly("FAT32"),
            MediaMode::Format if editable => {
                pick_list(FsChoice::ALL, Some(self.filesystem), Message::Filesystem)
                    .text_size(12)
                    .padding([2, 5])
                    .width(Fill)
                    .style(classic_pick_list)
                    .into()
            }
            MediaMode::Format => readonly(self.filesystem.to_string()),
        };
        let mut format = column![
            section_title("Format Options"),
            field("Volume label", label),
            field("File system", filesystem),
            checkbox(
                "Safely remove the drive when finished",
                self.eject_when_done
            )
            .size(14)
            .text_size(12)
            .on_toggle(Message::EjectWhenDone),
        ]
        .spacing(6);
        if self.media_mode == MediaMode::Windows {
            format = format.push(self.windows_options(editable));
        }
        format = format.push(
            button(
                text(if self.advanced_format {
                    "Hide advanced format options"
                } else {
                    "Show advanced format options"
                })
                .size(12),
            )
            .padding([1, 0])
            .style(button::text)
            .on_press_maybe(
                (!self.confirm_open && !self.show_log).then_some(Message::ToggleAdvancedFormat),
            ),
        );
        if self.advanced_format {
            format = format.push(
                checkbox("Verify written data by reading it back (always on)", true)
                    .size(14)
                    .text_size(12),
            );
            format = format.push(text(match self.media_mode {
                MediaMode::Raw => "Disk image mode copies the image exactly: its partitions, file systems, label and boot loaders. Compressed images are expanded in your cache first.",
                MediaMode::Windows => "Windows mode copies the installer into a FAT32 partition sized to the drive, splits a large install.wim, verifies every file, then asks you to confirm the write. MBR and GPT both boot with UEFI. Secure Boot depends on the original loaders and the PC's firmware.",
                MediaMode::Format => "Format mode builds an empty file system sized to the drive. Unused space is not written, so it is fast even on large drives. No boot loader is installed.",
            }).size(11));
            format = format.push(
                checkbox("Dark theme", self.dark)
                    .size(14)
                    .text_size(12)
                    .on_toggle(|_| Message::ToggleTheme),
            );
        }
        let mut content = column![drive, format].spacing(14);
        if self.media_mode == MediaMode::Raw
            && self
                .image
                .as_ref()
                .is_some_and(|image| !image.bootable_disk && image.kind != ImageKind::Compressed)
        {
            content = content.push(text("This image can't be written as-is: it has no USB boot layout. Use a vendor USB image or a hybrid ISO; for Windows ISOs choose \"Windows installer\".").size(12).style(text::danger));
        }
        if let Some(error) = self.error.as_ref().or(self.discovery_error.as_ref()) {
            content = content.push(
                container(
                    text(error.chars().take(600).collect::<String>())
                        .size(12)
                        .style(text::danger),
                )
                .padding(6)
                .width(Fill)
                .style(|theme: &Theme| container::Style {
                    border: iced::Border {
                        color: theme.palette().danger,
                        width: 1.0,
                        radius: 0.0.into(),
                    },
                    ..Default::default()
                }),
            );
        }
        let progress = self.running.as_ref().map(|running| &running.progress);
        let percent = progress
            .map(|value| {
                if value.total == 0 {
                    0.0
                } else {
                    (value.completed as f64 / value.total as f64 * 100.0).clamp(0.0, 100.0) as f32
                }
            })
            .unwrap_or(if self.done { 100.0 } else { 0.0 });
        let bar_label = if let Some(running) = &self.running {
            if running.cancel.is_cancelled() {
                "CANCELLING".into()
            } else {
                format!("{} {:.0}%", stage_name(running.progress.stage), percent)
            }
        } else if self.picking {
            "SELECT AN IMAGE".into()
        } else if self.inspecting {
            "ANALYZING IMAGE".into()
        } else if self.planning {
            "PREPARING".into()
        } else if self.unmounting {
            "UNMOUNTING".into()
        } else if self.error.is_some() || self.discovery_error.is_some() {
            "ERROR".into()
        } else if self.done {
            "DONE".into()
        } else {
            "READY".into()
        };
        let progress_widget = iced::widget::stack![
            progress_bar(0.0..=100.0, percent)
                .height(22)
                .style(classic_progress),
            container(text(bar_label).size(12).color(if percent > 55.0 {
                Color::WHITE
            } else {
                self.theme().palette().text
            }))
            .width(Fill)
            .height(22)
            .center_x(Fill)
            .center_y(22),
        ];
        let cancelling = self
            .running
            .as_ref()
            .is_some_and(|running| running.cancel.is_cancelled());
        let actions = row![
            button(text("Log").size(12))
                .padding([4, 7])
                .style(classic_button)
                .on_press_maybe((!self.confirm_open).then_some(Message::ToggleLog)),
            button(text("Refresh").size(12))
                .padding([4, 7])
                .style(classic_button)
                .on_press_maybe(
                    (!self.discovering && !self.confirm_open && !self.show_log)
                        .then_some(Message::Refresh)
                ),
            iced::widget::Space::with_width(Fill),
            button(text("START").size(12).center())
                .width(94)
                .padding([2, 8])
                .style(classic_button)
                .on_press_maybe(
                    (self.can_plan() && self.smoke_started.is_none() && !self.show_log)
                        .then_some(Message::MakePlan)
                ),
            button(
                text(if self.running.is_some() {
                    "CANCEL"
                } else {
                    "CLOSE"
                })
                .size(12)
                .center()
            )
            .width(94)
            .padding([2, 8])
            .style(classic_button)
            .on_press_maybe(
                (!cancelling && !self.confirm_open && !self.show_log).then_some(
                    if self.running.is_some() {
                        Message::Cancel
                    } else {
                        Message::CloseRequested
                    }
                )
            ),
        ]
        .spacing(8)
        .align_y(iced::Center);
        let bottom = if self.discovery_error.is_some() && self.error.is_none() && self.idle() {
            "Couldn't read the list of drives — reconnect the drive or press Refresh".into()
        } else if self.running.is_some()
            || self.planning
            || self.inspecting
            || self.unmounting
            || self.error.is_some()
        {
            ellipsize(&self.status, 64)
        } else if self.test_mode && !self.done {
            "Disposable file mode — creates new files only".into()
        } else {
            ellipsize(&self.hint(), 64)
        };
        let timing = self.running.as_ref().map(rate_and_eta).unwrap_or_default();
        let footer = column![
            section_title("Status"),
            progress_widget,
            container(actions).padding([6, 0])
        ]
        .spacing(12);
        let base: Element<'_, Message> = container(column![
            scrollable(container(content).padding(iced::Padding {
                top: 12.0,
                right: 14.0,
                bottom: 0.0,
                left: 14.0
            }))
            .id(scrollable::Id::new("workflow"))
            .width(Fill)
            .height(Fill),
            container(footer).padding([10, 14]),
            horizontal_rule(1),
            container(row![text(bottom).size(11).width(Fill), text(timing).size(11)].spacing(8))
                .padding([5, 7]),
        ])
        .width(Fill)
        .height(Fill)
        .style(dialog_background)
        .into();
        if self.confirm_open && self.plan.is_some() {
            modal(base, self.confirmation_view())
        } else if let Some(view) = &self.checksums {
            modal(base, self.checksum_view(view))
        } else if self.show_log {
            let lines = self.log.iter().fold(column![].spacing(6), |lines, line| {
                lines.push(text(line).size(11))
            });
            modal(
                base,
                column![
                    text("Log").size(17),
                    scrollable(lines).height(340),
                    row![
                        iced::widget::Space::with_width(Fill),
                        button(text("CLOSE").size(12))
                            .style(classic_button)
                            .padding([4, 18])
                            .on_press(Message::ToggleLog)
                    ],
                ]
                .spacing(12)
                .into(),
            )
        } else {
            base
        }
    }

    fn windows_options(&self, editable: bool) -> Element<'_, Message> {
        let option = |label: String, on: bool, which: WindowsOption| {
            let c = checkbox(label, on).size(14).text_size(12);
            if editable {
                c.on_toggle(move |v| Message::WindowsOption(which, v))
            } else {
                c
            }
        };
        let account = text_input("account name", &self.account_name)
            .size(12)
            .padding([1, 4])
            .width(150)
            .style(classic_input);
        let account = if editable && self.windows.local_account.is_some() {
            account.on_input(Message::AccountName)
        } else {
            account
        };
        let regional_label = match &self.host_locale {
            Some(tag) => format!("Use this computer's regional settings ({tag})"),
            None => "Use this computer's regional settings (not detected)".into(),
        };
        let mut regional = checkbox(regional_label, self.windows.regional.is_some())
            .size(14)
            .text_size(12);
        if editable && self.host_locale.is_some() {
            regional = regional.on_toggle(|v| Message::WindowsOption(WindowsOption::Regional, v));
        }
        let mut panel = column![
            text("Windows setup options").size(12).font(Font {
                weight: iced::font::Weight::Bold,
                ..Font::DEFAULT
            }),
            option(
                "Remove the TPM 2.0, Secure Boot and 4 GB RAM requirements".into(),
                self.windows.bypass_requirements,
                WindowsOption::Requirements
            ),
            option(
                "Allow setup without a Microsoft account".into(),
                self.windows.bypass_online_account,
                WindowsOption::OnlineAccount
            ),
            row![
                option(
                    "Create a local account named".into(),
                    self.windows.local_account.is_some(),
                    WindowsOption::LocalAccount
                ),
                account
            ]
            .spacing(8)
            .align_y(iced::Center),
            regional,
            option(
                "Decline optional data collection (privacy questions)".into(),
                self.windows.disable_data_collection,
                WindowsOption::DataCollection
            ),
            option(
                "Turn off automatic BitLocker drive encryption".into(),
                self.windows.disable_bitlocker,
                WindowsOption::BitLocker
            ),
        ]
        .spacing(5);
        if self.windows.local_account.is_some() {
            let name = self.account_name.trim();
            let note = if name.is_empty() {
                "Type an account name. Windows asks for its password at first sign-in.".to_owned()
            } else if let Err(e) = forgeusb_windows::unattend::validate_account(name) {
                format!("{e}")
            } else {
                "Windows asks for this account's password at first sign-in.".to_owned()
            };
            panel = panel.push(text(note).size(11).style(text::secondary));
        }
        container(panel)
            .padding(8)
            .width(Fill)
            .style(|theme: &Theme| container::Style {
                border: iced::Border {
                    color: Color::from_rgb8(170, 170, 170),
                    width: 1.0,
                    radius: 0.0.into(),
                },
                ..dialog_background(theme)
            })
            .into()
    }

    fn checksum_view<'a>(&'a self, view: &'a ChecksumView) -> Element<'a, Message> {
        let mut body = column![text("Checksums").size(18)].spacing(10);
        body = body.push(text(ellipsize(self.image_path.trim(), 70)).size(11));
        if view.computing {
            body = body.push(text("Reading the whole file…").size(12));
        } else if let Some(error) = &view.error {
            body = body.push(text(error).size(12).style(text::danger));
        }
        if let Some(sums) = &view.result {
            for (name, value) in [
                ("MD5", &sums.md5),
                ("SHA-1", &sums.sha1),
                ("SHA-256", &sums.sha256),
                ("SHA-512", &sums.sha512),
            ] {
                body = body.push(
                    column![
                        text(name).size(11),
                        text_input("", value)
                            .size(10)
                            .font(Font::MONOSPACE)
                            .padding([2, 4])
                            .style(classic_input),
                    ]
                    .spacing(2),
                );
            }
            body = body.push(
                text_input("Paste the publisher's checksum to compare", &view.expected)
                    .size(11)
                    .padding([3, 5])
                    .style(classic_input)
                    .on_input(Message::ExpectedChecksum),
            );
            if let Some(p) = &view.published
                && p.digest == view.expected
            {
                body = body.push(
                    text(format!("Found in {}", p.list.display()))
                        .size(11)
                        .style(text::secondary),
                );
            }
            if !view.expected.trim().is_empty() {
                body = body.push(match sums.matches(&view.expected) {
                    Some(alg) => text(format!("✓ {alg} matches — this is the published file."))
                        .size(12)
                        .style(text::success),
                    None => text("✗ No match — the file differs from the published one, or the checksum is for another file.")
                        .size(12)
                        .style(text::danger),
                });
            }
        }
        body.push(row![
            iced::widget::Space::with_width(Fill),
            button(text("CLOSE").size(12))
                .style(classic_button)
                .padding([4, 18])
                .on_press(Message::CloseChecksums)
        ])
        .into()
    }

    fn confirmation_view(&self) -> Element<'_, Message> {
        let plan = self.plan.as_ref().expect("confirmation requires a plan");
        let token = Self::confirmation_token(plan);
        let mut details = column![].spacing(6);
        let headline = match &plan.target_kind {
            TargetKind::Block(device) => {
                let mut lines = vec![format!(
                    "{} ({}, {})",
                    device_model(device),
                    drive_size(device.capacity),
                    device.path.display()
                )];
                for p in &device.partitions {
                    lines.push(format!(
                        "  • {} {} — {}",
                        p.label
                            .as_deref()
                            .map(|l| format!("\"{l}\""))
                            .unwrap_or_else(|| "partition".into()),
                        p.filesystem.as_deref().unwrap_or(""),
                        size(p.capacity)
                    ));
                }
                details = details.push(
                    text("Everything on this drive will be erased:")
                        .size(13)
                        .style(text::danger),
                );
                lines.join("\n")
            }
            TargetKind::TestFile => {
                details =
                    details.push(text("A new disposable image file will be created:").size(13));
                plan.target.display().to_string()
            }
        };
        details = details.push(text(headline).size(12));
        let what = match (self.media_mode, &self.prepared) {
            (MediaMode::Windows, Some(_)) => format!(
                "Windows installer from {}",
                file_name(self.image_path.trim())
            ),
            (MediaMode::Format, _) => {
                format!("Empty {} drive \"{}\"", self.filesystem, self.label.trim())
            }
            _ => format!(
                "{} ({})",
                file_name(self.image_path.trim()),
                kind_name(&plan.image)
            ),
        };
        details = details.push(
            text(format!(
                "Writes: {what}\n{} of data, then reads it all back to verify.",
                size(plan.image.transfer_bytes())
            ))
            .size(12),
        );
        if self.media_mode == MediaMode::Windows {
            let c = self.customization();
            for item in c.summary() {
                details = details.push(text(format!("Setup option: {item}")).size(11));
            }
            details = details.push(
                text("Windows Setup, physical boot and Secure Boot are not tested by ForgeUSB.")
                    .size(11)
                    .style(text::secondary),
            );
        }
        for warning in &plan.warnings {
            details = details.push(text(warning).size(10).style(text::secondary));
        }
        let confirm = text_input(&format!("Type {token} to confirm"), &self.confirmation)
            .id(text_input::Id::new("confirmation"))
            .size(12)
            .padding([5, 6])
            .style(classic_input)
            .on_input(Message::Confirmation)
            .on_submit(Message::Start);
        column![
            text(match plan.target_kind {
                TargetKind::Block(_) => "Erase this drive?",
                TargetKind::TestFile => "Create this file?",
            })
            .size(18),
            scrollable(container(details).padding(iced::Padding {
                right: 14.0,
                ..iced::Padding::ZERO
            }))
            .height(210),
            horizontal_rule(1),
            row![
                text("To confirm, type").size(12),
                text(token.clone()).size(12).font(Font::MONOSPACE),
                text("below:").size(12)
            ]
            .spacing(4),
            confirm,
            row![
                iced::widget::Space::with_width(Fill),
                button(text("START").size(12))
                    .width(94)
                    .padding([2, 8])
                    .style(classic_button)
                    .on_press_maybe(self.can_start().then_some(Message::Start)),
                button(text("CANCEL").size(12))
                    .width(94)
                    .padding([2, 8])
                    .style(classic_button)
                    .on_press(Message::DismissPlan),
            ]
            .spacing(8),
        ]
        .spacing(12)
        .into()
    }
}

fn completion_summary(report: &forgeusb_linux::Report) -> String {
    let mut summary = format!(
        "Verified {} ({} written) · SHA-256 {}",
        size(report.bytes),
        size(if report.written == 0 {
            report.bytes
        } else {
            report.written
        }),
        &report.sha256[..16.min(report.sha256.len())]
    );
    for note in &report.notes {
        summary += &format!(" · {note}");
    }
    summary
}

fn rate_and_eta(running: &Running) -> String {
    let p = &running.progress;
    let elapsed = running.stage_started.elapsed().as_secs_f64();
    let total = running.started.elapsed().as_secs();
    let clock = format!(
        "{:02}:{:02}:{:02}",
        total / 3600,
        total / 60 % 60,
        total % 60
    );
    if elapsed < 1.5 || p.completed == 0 || p.total == 0 {
        return clock;
    }
    let rate = p.completed as f64 / elapsed;
    let remaining = (p.total.saturating_sub(p.completed)) as f64 / rate;
    let eta = if remaining >= 90.0 {
        format!("{:.0} min left", remaining / 60.0)
    } else {
        format!("{remaining:.0} s left")
    };
    match p.stage {
        Stage::Writing | Stage::Verifying => {
            format!("{}/s · {eta} · {clock}", size(rate as u64))
        }
        _ => clock,
    }
}

fn device_model(d: &Device) -> String {
    let model = [d.vendor.trim(), d.model.trim()]
        .into_iter()
        .filter(|s| !s.is_empty() && *s != "Unknown")
        .collect::<Vec<_>>()
        .join(" ");
    if model.is_empty() {
        "USB drive".into()
    } else {
        model
    }
}

fn drive_size(n: u64) -> String {
    let gb = n as f64 / 1e9;
    if gb >= 10.0 {
        format!("{gb:.0} GB")
    } else {
        format!("{gb:.1} GB")
    }
}

fn kind_name(image: &ImageInfo) -> String {
    match image.kind {
        ImageKind::Disk => "Disk image",
        ImageKind::IsoHybrid => "Hybrid ISO",
        ImageKind::OpticalIso if image.windows => "Windows installer ISO",
        ImageKind::OpticalIso => "ISO (not USB-bootable as-is)",
        ImageKind::Udf if image.windows => "Windows installer ISO",
        ImageKind::Udf => "UDF image",
        ImageKind::Compressed => "Compressed image",
        ImageKind::Unknown => "Unrecognized file",
    }
    .into()
}

fn field<'a>(label: &'a str, control: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    column![text(label).size(12), control.into()]
        .spacing(3)
        .width(Fill)
        .into()
}

fn readonly<'a>(value: impl Into<String>) -> Element<'a, Message> {
    let value = value.into();
    path_box(value, "", None)
}

/// A path shown to fit its box, keeping the file name visible. Long text
/// inputs overdraw neighbouring controls with the tiny-skia renderer, so
/// paths are displayed rather than edited in place.
fn path_box<'a>(
    value: String,
    placeholder: &'a str,
    on_press: Option<Message>,
) -> Element<'a, Message> {
    let enabled = on_press.is_some();
    let content = iced::widget::responsive(move |size| {
        // Conservative average advance for 12 px Noto Sans.
        let fit = ((size.width / 6.8) as usize).max(12);
        let (shown, secondary) = if value.is_empty() {
            (placeholder.to_owned(), true)
        } else {
            (middle_ellipsize(&value, fit), !enabled)
        };
        let label = text(shown).size(12).wrapping(text::Wrapping::None);
        if secondary {
            label.style(text::secondary).into()
        } else {
            label.into()
        }
    });
    let boxed = container(content)
        .padding([2, 5])
        .width(Fill)
        .height(20)
        .style(move |theme: &Theme| {
            let input = classic_input(
                theme,
                if enabled {
                    text_input::Status::Active
                } else {
                    text_input::Status::Disabled
                },
            );
            container::Style {
                background: Some(input.background),
                border: input.border,
                ..Default::default()
            }
        });
    let area = iced::widget::mouse_area(boxed);
    match on_press {
        Some(message) => area
            .on_press(message)
            .interaction(iced::mouse::Interaction::Pointer)
            .into(),
        None => area.into(),
    }
}

fn file_name(path: &str) -> String {
    let name = std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_owned());
    ellipsize(&name, 60)
}

fn middle_ellipsize(value: &str, limit: usize) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= limit {
        return value.to_owned();
    }
    let tail = (limit * 2 / 3).max(1);
    let head = limit.saturating_sub(tail + 1);
    let mut result: String = chars[..head].iter().collect();
    result.push('…');
    result.extend(&chars[chars.len() - tail..]);
    result
}

fn section_title<'a>(title: &'a str) -> Element<'a, Message> {
    row![
        text(title).size(19).font(Font {
            weight: iced::font::Weight::Bold,
            ..Font::DEFAULT
        }),
        horizontal_rule(1).style(|theme: &Theme| iced::widget::rule::Style {
            color: theme.palette().text,
            width: 1,
            radius: 0.0.into(),
            fill_mode: iced::widget::rule::FillMode::Full,
        }),
    ]
    .spacing(10)
    .align_y(iced::Center)
    .into()
}

fn dialog_background(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(theme.palette().background.into()),
        text_color: Some(theme.palette().text),
        ..Default::default()
    }
}

fn modal<'a>(base: Element<'a, Message>, dialog: Element<'a, Message>) -> Element<'a, Message> {
    iced::widget::stack![
        base,
        iced::widget::opaque(
            container(
                container(dialog)
                    .padding(16)
                    .width(Fill)
                    .max_width(447)
                    .style(|theme: &Theme| container::Style {
                        border: iced::Border {
                            color: Color::from_rgb8(135, 135, 135),
                            width: 1.0,
                            radius: 0.0.into()
                        },
                        ..dialog_background(theme)
                    })
            )
            .padding(12)
            .center_x(Fill)
            .center_y(Fill)
            .style(|_: &Theme| container::Style {
                background: Some(Color::from_rgba(0.0, 0.0, 0.0, 0.16).into()),
                ..Default::default()
            })
        )
    ]
    .into()
}

fn classic_button(theme: &Theme, status: button::Status) -> button::Style {
    let mut style = button::secondary(theme, status);
    let dark = theme.palette().background.r < 0.5;
    let disabled = matches!(status, button::Status::Disabled);
    style.background = Some(
        if dark {
            Color::from_rgb8(63, 63, 63)
        } else {
            Color::from_rgb8(225, 225, 225)
        }
        .into(),
    );
    style.text_color = if disabled {
        if dark {
            Color::from_rgb8(145, 145, 145)
        } else {
            Color::from_rgb8(131, 131, 131)
        }
    } else {
        theme.palette().text
    };
    style.border = iced::Border {
        color: if matches!(status, button::Status::Hovered | button::Status::Pressed) {
            theme.palette().primary
        } else if disabled {
            Color::from_rgb8(200, 200, 200)
        } else {
            Color::from_rgb8(160, 160, 160)
        },
        width: 1.0,
        radius: 0.0.into(),
    };
    style.shadow = iced::Shadow::default();
    style
}

fn classic_input(theme: &Theme, status: text_input::Status) -> text_input::Style {
    let mut style = text_input::default(theme, status);
    let dark = theme.palette().background.r < 0.5;
    let disabled = matches!(status, text_input::Status::Disabled);
    style.background = if dark {
        Color::from_rgb8(44, 44, 44)
    } else if disabled {
        Color::from_rgb8(246, 246, 246)
    } else {
        Color::WHITE
    }
    .into();
    style.border = iced::Border {
        color: if matches!(status, text_input::Status::Focused) {
            theme.palette().primary
        } else if disabled {
            Color::from_rgb8(214, 214, 214)
        } else {
            Color::from_rgb8(166, 166, 166)
        },
        width: 1.0,
        radius: 0.0.into(),
    };
    style.value = if disabled {
        if dark {
            Color::from_rgb8(170, 170, 170)
        } else {
            Color::from_rgb8(112, 112, 112)
        }
    } else {
        theme.palette().text
    };
    style.placeholder = Color::from_rgb8(120, 120, 120);
    style
}

fn classic_pick_list(theme: &Theme, status: pick_list::Status) -> pick_list::Style {
    pick_list::Style {
        text_color: theme.palette().text,
        placeholder_color: Color::from_rgb8(112, 112, 112),
        handle_color: theme.palette().text,
        background: if theme.palette().background.r < 0.5 {
            Color::from_rgb8(44, 44, 44)
        } else {
            Color::WHITE
        }
        .into(),
        border: iced::Border {
            color: if matches!(
                status,
                pick_list::Status::Hovered | pick_list::Status::Opened
            ) {
                theme.palette().primary
            } else {
                Color::from_rgb8(166, 166, 166)
            },
            width: 1.0,
            radius: 0.0.into(),
        },
    }
}

fn classic_progress(theme: &Theme) -> progress_bar::Style {
    progress_bar::Style {
        background: if theme.palette().background.r < 0.5 {
            Color::from_rgb8(48, 48, 48)
        } else {
            Color::from_rgb8(230, 230, 230)
        }
        .into(),
        bar: Color::from_rgb8(6, 176, 37).into(),
        border: iced::Border {
            color: Color::from_rgb8(180, 180, 180),
            width: 1.0,
            radius: 0.0.into(),
        },
    }
}

fn ellipsize(value: &str, limit: usize) -> String {
    let mut characters = value.chars();
    let mut result: String = characters.by_ref().take(limit).collect();
    if characters.next().is_some() {
        result.push('…');
    }
    result
}

fn stage_name(stage: Stage) -> &'static str {
    match stage {
        Stage::Inspecting => "Analyzing",
        Stage::Revalidating => "Revalidating",
        Stage::Writing => "Writing",
        Stage::Flushing => "Flushing",
        Stage::Verifying => "Verifying",
        Stage::Complete => "Complete",
    }
}

/// Only unprivileged, fixed system dialog executables are launched. No shell.
/// A cancelled/closed parent waits for the owned child to be killed and reaped.
fn select_image(cancel: &Cancellation) -> Result<Option<String>, String> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute() && path.is_dir())
        .unwrap_or_else(|| PathBuf::from("/"));
    let downloads = home.join("Downloads");
    let initial = if downloads.is_dir() { downloads } else { home };
    let mut command = if std::path::Path::new("/usr/bin/kdialog").is_file() {
        let mut command = Command::new("/usr/bin/kdialog");
        command
            .args(["--title", "Select an image — ForgeUSB", "--getopenfilename"])
            .arg(&initial)
            .arg("*.iso *.img *.dd *.raw|Disk images\n*|All files");
        command
    } else if std::path::Path::new("/usr/bin/zenity").is_file() {
        let mut command = Command::new("/usr/bin/zenity");
        command.args([
            "--file-selection",
            "--title=Select an image — ForgeUSB",
            "--file-filter=Disk images | *.iso *.img *.dd *.raw",
            "--file-filter=All files | *",
        ]);
        command.arg("--filename").arg(initial.join(""));
        command
    } else {
        return Err("No native file chooser found. Install kdialog or zenity, or paste an absolute image path into Boot selection and press Enter.".into());
    };
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("Cannot open file chooser: {error}"))?;
    let started = Instant::now();
    loop {
        if cancel.is_cancelled() || started.elapsed() >= Duration::from_secs(300) {
            let _ = child.kill();
            let _ = child.wait();
            return if cancel.is_cancelled() {
                Ok(None)
            } else {
                Err("File chooser timed out. Please select the image again.".into())
            };
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                if status.code() == Some(1) {
                    return Ok(None);
                }
                if !status.success() {
                    return Err(format!(
                        "File chooser exited with {status}. Paste the image path into Boot selection instead."
                    ));
                }
                let mut output = Vec::new();
                child
                    .stdout
                    .take()
                    .ok_or("File chooser has no output")?
                    .take(65_537)
                    .read_to_end(&mut output)
                    .map_err(|error| error.to_string())?;
                if output.len() > 65_536 {
                    return Err("File chooser returned an oversized path.".into());
                }
                let output =
                    String::from_utf8(output).map_err(|_| "The image path must be valid UTF-8.")?;
                let path = output.strip_suffix('\n').unwrap_or(&output);
                if path.is_empty() {
                    return Ok(None);
                }
                if path.contains(['\n', '\r']) || !std::path::Path::new(path).is_absolute() {
                    return Err("Select one image with an absolute, single-line file path.".into());
                }
                return Ok(Some(path.to_owned()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("File chooser failed: {error}"));
            }
        }
    }
}

fn shortcut(key: iced::keyboard::Key, modifiers: iced::keyboard::Modifiers) -> Option<Message> {
    use iced::keyboard::{Key, key::Named};
    match key.as_ref() {
        Key::Named(Named::Tab) => Some(Message::FocusNext(modifiers.shift())),
        Key::Named(Named::Escape) => Some(Message::Cancel),
        Key::Named(Named::F8) => Some(Message::StartRequested),
        Key::Named(Named::ArrowDown) if modifiers.alt() => Some(Message::SelectRelative(1)),
        Key::Named(Named::ArrowUp) if modifiers.alt() => Some(Message::SelectRelative(-1)),
        Key::Character("o" | "O") if modifiers.control() => Some(Message::FocusImage),
        Key::Character("p" | "P") if modifiers.control() => Some(Message::MakePlan),
        Key::Character("r" | "R") if modifiers.control() => Some(Message::Refresh),
        Key::Character("f" | "F") if modifiers.alt() => Some(Message::ToggleTestMode),
        Key::Character("t" | "T") if modifiers.alt() => Some(Message::ToggleTheme),
        Key::Character("l" | "L") if modifiers.alt() => Some(Message::ToggleLog),
        Key::Character("a" | "A") if modifiers.alt() => Some(Message::ToggleAdvanced),
        _ => None,
    }
}

fn size(bytes: u64) -> String {
    if bytes >= 1 << 30 {
        format!("{:.2} GiB", bytes as f64 / (1_u64 << 30) as f64)
    } else if bytes >= 1 << 20 {
        format!("{:.1} MiB", bytes as f64 / (1_u64 << 20) as f64)
    } else if bytes >= 1 << 10 {
        format!("{:.1} KiB", bytes as f64 / (1_u64 << 10) as f64)
    } else {
        format!("{bytes} B")
    }
}

/// The desktop never opens a physical block device for writing. A fixed,
/// root-owned Polkit helper executes the same Plan protocol as the CLI.
fn helper_write(
    plan: &Plan,
    confirmation: &str,
    cancel: &Cancellation,
    tx: &SyncSender<WorkerEvent>,
) -> Result<String, String> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::symlink_metadata(HELPER)
        .map_err(|_| format!("Install the ForgeUSB helper and Polkit policy using packaging/ installation instructions. Expected {HELPER}. The GUI must remain unprivileged."))?;
    if !metadata.is_file() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
        return Err(format!(
            "Refusing {HELPER}: it must be a root-owned regular file without group/other write access."
        ));
    }
    let directory = std::fs::symlink_metadata("/usr/lib/forgeusb")
        .map_err(|e| format!("Cannot validate helper installation directory: {e}"))?;
    if !directory.is_dir() || directory.uid() != 0 || directory.mode() & 0o022 != 0 {
        return Err("Helper installation directory must be a root-owned directory without group/other write access or symlinks".into());
    }
    cancel.check().map_err(|e| e.to_string())?;
    let mut child = Command::new("/usr/bin/pkexec")
        .arg(HELPER)
        .args([
            "write",
            "--plan-json",
            forgeusb_linux::HELPER_PROTOCOL_ARGUMENT,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Cannot start Polkit helper: {e}"))?;
    let mut input = child.stdin.take().ok_or("Helper stdin unavailable")?;
    let request = serde_json::to_string(&WriteRequest {
        plan: plan.clone(),
        confirmation: confirmation.into(),
    })
    .map_err(|e| e.to_string())?;
    if writeln!(input, "{request}")
        .and_then(|()| input.flush())
        .is_err()
    {
        let _ = child.kill();
        let _ = child.wait();
        return Err("Cannot send confirmed operation to helper".into());
    }
    let stdout = child.stdout.take().ok_or("Helper stdout unavailable")?;
    let stderr = child.stderr.take().ok_or("Helper stderr unavailable")?;
    let (line_tx, line_rx) = mpsc::sync_channel(256);
    let output_reader = std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let mut line = String::new();
            // A trusted helper should emit small records; cap protocol records.
            match reader.by_ref().take(65_537).read_line(&mut line) {
                Ok(0) => break,
                Ok(_) if line.len() > 65_536 => {
                    let _ = line_tx.send(Err("Helper emitted an oversized record".into()));
                    break;
                }
                Ok(_) => {
                    if line_tx.send(Ok(line)).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    let _ = line_tx.send(Err(format!("Helper output error: {e}")));
                    break;
                }
            }
        }
    });
    let error_reader = std::thread::spawn(move || {
        let mut result = String::new();
        let _ = stderr.take(8192).read_to_string(&mut result);
        result
    });
    let mut sent_cancel = false;
    let mut complete = None;
    let mut failure = None;
    loop {
        if (cancel.is_cancelled() || failure.is_some()) && !sent_cancel {
            let _ = writeln!(input, "cancel").and_then(|()| input.flush());
            sent_cancel = true;
        }
        match line_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Ok(line)) => match serde_json::from_str::<HelperEvent>(&line) {
                Ok(HelperEvent::Progress(progress)) => {
                    let _ = tx.try_send(WorkerEvent::Progress(progress));
                }
                Ok(HelperEvent::Complete(report)) => {
                    if report.verified
                        && report.bytes == plan.image.size
                        && report.sha256 == plan.image.sha256
                    {
                        complete = Some(completion_summary(&report));
                    } else {
                        failure = Some("Helper completion does not match the confirmed image or lacks verification".into());
                    }
                }
                Ok(HelperEvent::Error(error)) => failure = Some(error),
                Err(e) => failure = Some(format!("Invalid helper protocol: {e}")),
            },
            Ok(Err(error)) => failure = Some(error),
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if child.try_wait().map_err(|e| e.to_string())?.is_some() {
                    // Reader still owns queued records, so drain through EOF.
                    continue;
                }
            }
        }
    }
    drop(input);
    let status = child.wait().map_err(|e| e.to_string())?;
    let _ = output_reader.join();
    let stderr = error_reader.join().unwrap_or_default();
    if let Some(error) = failure {
        return Err(error);
    }
    if !status.success() {
        return Err(format!(
            "Polkit helper exited with {status}. {} The target may be incomplete.",
            stderr.trim()
        ));
    }
    complete.ok_or_else(|| {
        "Helper exited without a verified completion record. The target may be incomplete.".into()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use forgeusb_device::Identity;
    use forgeusb_image::{FileStamp, ImageKind};

    fn device() -> Device {
        Device {
            path: PathBuf::from("/dev/forgeusb-test-never-open"),
            model: "Synthetic USB".into(),
            vendor: "Fixture".into(),
            capacity: 4096,
            identity: Identity {
                major: 8,
                minor: 255,
                logical_sector_size: 512,
                sysfs_path: PathBuf::from("/sys/synthetic/usb/fixture"),
                capacity: 4096,
                serial: Some("test-only".into()),
                wwid: None,
                topology: Vec::new(),
            },
            eligible: true,
            reasons: Vec::new(),
            mounts: Vec::new(),
            partitions: Vec::new(),
            removable: true,
            transport: "usb".into(),
            blocked_only_by_mounts: false,
        }
    }

    fn plan() -> Plan {
        Plan {
            image: ImageInfo {
                path: PathBuf::from("/tmp/synthetic-never-open.img"),
                size: 512,
                sha256: "00".repeat(32),
                kind: ImageKind::Disk,
                bootable_disk: true,
                iso9660: false,
                udf: false,
                el_torito: false,
                windows: false,
                warnings: Vec::new(),
                coverage: Coverage::Full,
                ranges: Vec::new(),
                stamp: FileStamp {
                    device: 0,
                    inode: 0,
                    size: 512,
                    mtime: 0,
                    mtime_ns: 0,
                    ctime: 0,
                    ctime_ns: 0,
                },
            },
            target: PathBuf::from("/tmp/synthetic-output-never-open.img"),
            capacity: 512,
            confirmation: "CREATE /tmp/synthetic-output-never-open.img 512 bytes".into(),
            warnings: Vec::new(),
            target_kind: TargetKind::TestFile,
            strategy: "raw_image_with_full_readback".into(),
        }
    }

    #[test]
    fn successful_discovery_clears_only_the_discovery_error() {
        let mut state = State::empty(Options::default());
        state.error = Some("Earlier operation failed".into());
        state.worker_event(WorkerEvent::Devices(Err("Transient sysfs failure".into())));
        assert!(state.discovery_error.is_some());
        assert!(state.choices.is_empty());
        state.worker_event(WorkerEvent::Devices(Ok(vec![device()])));
        assert!(state.discovery_error.is_none());
        assert_eq!(state.error.as_deref(), Some("Earlier operation failed"));
        assert_eq!(state.choices.len(), 1);
        assert!(state.selected.is_none());
    }

    #[test]
    fn discovery_failure_revokes_physical_selection_and_review() {
        let mut state = State::empty(Options::default());
        state.worker_event(WorkerEvent::Devices(Ok(vec![device()])));
        state.selected = Some(state.choices[0].clone());
        state.plan = Some(plan());
        state.confirm_open = true;
        state.confirmation = "stale approval".into();
        state.worker_event(WorkerEvent::Devices(Err("Transient sysfs failure".into())));
        assert!(state.selected.is_none());
        assert!(state.plan.is_none());
        assert!(!state.confirm_open);
        assert!(state.confirmation.is_empty());
        assert!(!state.can_start());
        state.worker_event(WorkerEvent::Devices(Ok(vec![device()])));
        assert!(state.discovery_error.is_none());
        assert!(state.selected.is_none());
        assert!(!state.can_start());
    }

    #[test]
    fn discovering_eligible_devices_never_selects_one() {
        let mut state = State::empty(Options::default());
        state.worker_event(WorkerEvent::Devices(Ok(vec![device()])));
        assert_eq!(state.choices.len(), 1);
        assert!(state.selected.is_none());
        assert!(!state.can_plan());
        assert!(!state.can_start());
    }

    #[test]
    fn start_requires_exact_confirmation_and_smoke_mode_forbids_it() {
        let mut state = State::empty(Options::default());
        let plan = plan();
        state.confirmation = "CREAT".into();
        state.plan = Some(plan.clone());
        state.confirm_open = true;
        assert!(!state.can_start());
        // The long engine confirmation is not what the user types.
        state.confirmation = plan.confirmation;
        assert!(!state.can_start());
        state.confirmation = " CREATE ".into();
        assert!(state.can_start());
        state.smoke_started = Some(Instant::now());
        assert!(!state.can_start());
    }

    #[test]
    fn disappeared_device_invalidates_selection_plan_and_confirmation() {
        let mut state = State::empty(Options::default());
        let device = device();
        state.devices = vec![device.clone()];
        state.selected = Some(DeviceChoice {
            path: device.path,
            label: "Fixture".into(),
        });
        state.plan = Some(plan());
        state.confirmation = "previous approval".into();
        state.worker_event(WorkerEvent::Devices(Ok(Vec::new())));
        assert!(state.selected.is_none());
        assert!(state.plan.is_none());
        assert!(state.confirmation.is_empty());
    }

    #[test]
    fn pending_plan_is_rejected_after_target_changes() {
        let mut state = State::empty(Options::default());
        state.test_mode = true;
        state.test_path = "/tmp/a-different-target.img".into();
        state.planning = true;
        state.worker_event(WorkerEvent::Planned(Ok(Box::new(plan()))));
        assert!(!state.planning);
        assert!(state.plan.is_none());
        assert!(!state.can_start());
    }
    #[test]
    fn dismissing_confirmation_revokes_the_plan() {
        let mut state = State::empty(Options::default());
        state.confirmation = "CREATE".into();
        state.plan = Some(plan());
        state.confirm_open = true;
        assert!(state.can_start());
        let _ = state.update(Message::DismissPlan);
        assert!(!state.confirm_open);
        assert!(state.plan.is_none());
        assert!(state.confirmation.is_empty());
        assert!(state.running.is_none());
        assert!(!state.can_start());
    }

    #[test]
    fn picking_a_source_blocks_planning_and_close_cancels_the_picker() {
        let mut state = State::empty(Options::default());
        state.test_mode = true;
        state.test_path = "/tmp/never-written-picker-test.img".into();
        state.image_path = "/tmp/never-opened-source.img".into();
        assert!(state.can_plan());
        state.picking = true;
        assert!(!state.can_plan());
        let _ = state.update(Message::CloseRequested);
        assert!(state.picker_cancel.is_cancelled());
        assert!(state.exit_after_operation);
        assert!(state.running.is_none());
    }

    #[test]
    fn windows_mode_change_revokes_review_and_smoke_cannot_prepare() {
        let mut state = State::empty(Options::default());
        state.image_path = "/tmp/synthetic-windows.iso".into();
        state.test_mode = true;
        state.test_path = "/tmp/never-created-windows-copy.img".into();
        state.plan = Some(plan());
        state.confirmation = "stale confirmation".into();
        // Settings are unavailable until the existing review is dismissed.
        state.confirm_open = true;
        let _ = state.update(Message::MediaMode(MediaMode::Windows));
        assert_eq!(state.media_mode, MediaMode::Raw);
        let _ = state.update(Message::DismissPlan);
        let _ = state.update(Message::MediaMode(MediaMode::Windows));
        assert_eq!(state.media_mode, MediaMode::Windows);
        assert!(state.plan.is_none() && state.confirmation.is_empty());
        assert!(state.can_plan());
        state.smoke_started = Some(Instant::now());
        assert!(!state.can_plan());
        assert!(!state.can_start());
    }

    #[test]
    fn close_during_preparation_cancels_before_any_target_review() {
        let mut state = State::empty(Options::default());
        let cancel = Cancellation::default();
        state.media_mode = MediaMode::Windows;
        state.planning = true;
        state.running = Some(Running {
            cancel: cancel.clone(),
            progress: forgeusb_core::progress(Stage::Inspecting, 0, 0, "Preparing"),
            started: Instant::now(),
            stage_started: Instant::now(),
        });
        let _ = state.update(Message::CloseRequested);
        assert!(cancel.is_cancelled());
        assert!(state.exit_after_operation);
        state.worker_event(WorkerEvent::Prepared(Err("cancelled".into())));
        assert!(state.plan.is_none() && state.prepared.is_none());
        assert!(state.running.is_none() && !state.planning);
        assert!(!state.confirm_open);
    }

    #[test]
    fn queued_preparation_success_cannot_override_cancellation() {
        let mut state = State::empty(Options::default());
        let cancel = Cancellation::default();
        cancel.cancel();
        state.planning = true;
        state.running = Some(Running {
            cancel,
            progress: forgeusb_core::progress(Stage::Writing, 1, 1, "Prepared"),
            started: Instant::now(),
            stage_started: Instant::now(),
        });
        let prepared = PreparedMedia {
            _directory: tempfile::tempdir().unwrap(),
            image: plan().image,
            sized_for: None,
            summary: String::new(),
        };
        state.worker_event(WorkerEvent::Prepared(Ok(prepared)));
        assert!(state.prepared.is_none() && state.plan.is_none());
        assert!(!state.confirm_open && !state.planning && state.running.is_none());
        assert!(state.status.starts_with("Preparation cancelled"));
    }

    #[test]
    fn disconnect_during_preparation_requires_explicit_reselection() {
        let mut state = State::empty(Options::default());
        state.worker_event(WorkerEvent::Devices(Ok(vec![device()])));
        state.selected = Some(state.choices[0].clone());
        state.planning = true;
        state.running = Some(Running {
            cancel: Cancellation::default(),
            progress: forgeusb_core::progress(Stage::Writing, 0, 0, "Preparing"),
            started: Instant::now(),
            stage_started: Instant::now(),
        });
        state.worker_event(WorkerEvent::Devices(Ok(Vec::new())));
        assert!(state.selected.is_none());
        assert!(state.plan.is_none() && !state.confirm_open);
        state.worker_event(WorkerEvent::Devices(Ok(vec![device()])));
        assert!(state.selected.is_none());
    }

    #[test]
    fn physical_confirmation_is_the_drive_short_name() {
        let mut plan = plan();
        let mut device = device();
        device.path = PathBuf::from("/dev/sdz");
        plan.target = device.path.clone();
        plan.target_kind = TargetKind::Block(Box::new(device));
        assert_eq!(State::confirmation_token(&plan), "sdz");
    }

    #[test]
    fn mounted_usb_drive_is_offered_but_cannot_be_planned_until_unmounted() {
        let mut state = State::empty(Options::default());
        let mut mounted = device();
        mounted.eligible = false;
        mounted.blocked_only_by_mounts = true;
        mounted.reasons = vec!["mounted".into()];
        let mut internal = device();
        internal.path = PathBuf::from("/dev/forgeusb-internal-never-open");
        internal.eligible = false;
        internal.reasons = vec!["not USB".into()];
        state.worker_event(WorkerEvent::Devices(Ok(vec![mounted, internal])));
        assert_eq!(state.choices.len(), 1);
        assert!(state.choices[0].label.ends_with("(mounted)"));
        state.image_path = "/tmp/never-opened-source.img".into();
        let _ = state.update(Message::ChooseDevice(state.choices[0].clone()));
        assert!(state.selected.is_some());
        assert!(!state.can_plan());
        assert!(state.hint().contains("Unmount"));
    }

    #[test]
    fn format_mode_needs_no_source_and_always_prepares() {
        let mut state = State::empty(Options::default());
        state.test_mode = true;
        state.test_path = "/tmp/never-created-format.img".into();
        let _ = state.update(Message::MediaMode(MediaMode::Format));
        assert!(state.needs_preparation());
        assert!(state.can_plan());
        let _ = state.update(Message::Filesystem(FsChoice::Fat32));
        let _ = state.update(Message::Label("my stick/2026".into()));
        assert_eq!(state.label, "MY STICK_20");
        let _ = state.update(Message::Filesystem(FsChoice::Ext4));
        let _ = state.update(Message::Label("a very long ext4 label".into()));
        assert_eq!(state.label, "a very long ext4");
    }

    #[test]
    fn windows_detection_switches_mode_and_account_name_is_applied() {
        let mut state = State::empty(Options::default());
        let mut image = plan().image;
        image.kind = ImageKind::Udf;
        image.bootable_disk = false;
        image.windows = true;
        state.inspecting = true;
        state.worker_event(WorkerEvent::Inspected(Ok(image)));
        assert_eq!(state.media_mode, MediaMode::Windows);
        let _ = state.update(Message::WindowsOption(WindowsOption::LocalAccount, true));
        let _ = state.update(Message::AccountName("  alex ".into()));
        assert_eq!(state.customization().local_account.as_deref(), Some("alex"));
        let _ = state.update(Message::WindowsOption(WindowsOption::LocalAccount, false));
        assert!(state.customization().local_account.is_none());
    }

    #[test]
    fn long_paths_keep_their_file_name() {
        let path = "/home/user/Downloads/very/deeply/nested/folder/Win11_24H2_English_x64.iso";
        let shown = middle_ellipsize(path, 40);
        assert_eq!(shown.chars().count(), 40);
        assert!(shown.ends_with("Win11_24H2_English_x64.iso"));
        assert_eq!(middle_ellipsize("/short.img", 40), "/short.img");
    }

    #[test]
    fn rate_estimate_needs_progress() {
        let running = Running {
            cancel: Cancellation::default(),
            progress: forgeusb_core::progress(Stage::Writing, 0, 100, "Writing"),
            started: Instant::now(),
            stage_started: Instant::now(),
        };
        assert_eq!(rate_and_eta(&running), "00:00:00");
    }
}
