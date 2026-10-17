//! Unprivileged desktop shell around the shared planning and execution engine.
//! Blocking I/O always runs on workers; the event loop only renders and polls.
use forgeusb_core::{Cancellation, Progress, Stage};
use forgeusb_device::Device;
use forgeusb_image::ImageInfo;
use forgeusb_linux::{HelperEvent, Plan, TargetKind, WriteRequest};
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

fn main() -> iced::Result {
    // This is a Linux desktop application. Only the narrow helper may be root.
    if unsafe { libc::geteuid() } == 0 {
        eprintln!(
            "ForgeUSB refuses to run its GUI as root. Launch it as your desktop user; physical writes use Polkit."
        );
        std::process::exit(2);
    }
    let options = Options::parse();
    iced::application("ForgeUSB 0.1.0", State::update, State::view)
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
            size: (475.0, 560.0).into(),
            min_size: Some((440.0, 540.0).into()),
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
                "--help" | "-h" => {
                    println!(
                        "ForgeUSB desktop\n\n  --image PATH          Prefill the source path\n  --test-output PATH    Prefill a NEW disposable output file\n  --smoke-test          Open for ten seconds without starting an operation\n\nThe GUI must run as your normal desktop user."
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

#[derive(Debug, Clone)]
enum Message {
    Tick,
    Refresh,
    ImagePath(String),
    BrowseImage,
    Inspect,
    ChooseDevice(DeviceChoice),
    TestMode(bool),
    TestPath(String),
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
    Planned(Result<Box<Plan>, String>),
    Progress(Progress),
    Finished(Result<String, String>),
}

struct Running {
    cancel: Cancellation,
    progress: Progress,
    started: Instant,
}

struct State {
    image_path: String,
    image: Option<ImageInfo>,
    devices: Vec<Device>,
    choices: Vec<DeviceChoice>,
    selected: Option<DeviceChoice>,
    test_mode: bool,
    test_path: String,
    plan: Option<Plan>,
    confirmation: String,
    running: Option<Running>,
    inspecting: bool,
    planning: bool,
    discovering: bool,
    dark: bool,
    show_log: bool,
    advanced: bool,
    advanced_format: bool,
    confirm_open: bool,
    picking: bool,
    picker_cancel: Cancellation,
    error: Option<String>,
    status: String,
    log: VecDeque<String>,
    tx: SyncSender<WorkerEvent>,
    rx: Receiver<WorkerEvent>,
    ticks: u64,
    smoke_started: Option<Instant>,
    exit_after_operation: bool,
    focus_preview: bool,
}

impl State {
    fn new(options: Options) -> Self {
        let mut state = Self::empty(options);
        state.append_log("ForgeUSB started as an unprivileged desktop user.");
        state.refresh();
        if !state.image_path.trim().is_empty() {
            let _ = state.update(Message::Inspect);
        }
        state
    }

    fn empty(options: Options) -> Self {
        let (tx, rx) = mpsc::sync_channel(256);
        Self {
            image_path: options.image,
            image: None,
            devices: Vec::new(),
            choices: Vec::new(),
            selected: None,
            test_mode: !options.test_output.is_empty(),
            test_path: options.test_output,
            plan: None,
            confirmation: String::new(),
            running: None,
            inspecting: false,
            planning: false,
            discovering: false,
            dark: false,
            show_log: false,
            advanced: false,
            advanced_format: false,
            confirm_open: false,
            picking: false,
            picker_cancel: Cancellation::default(),
            error: None,
            status: "Ready.".into(),
            log: VecDeque::new(),
            tx,
            rx,
            ticks: 0,
            smoke_started: options.smoke.then(Instant::now),
            exit_after_operation: false,
            focus_preview: false,
        }
    }

    fn append_log(&mut self, line: impl Into<String>) {
        self.log.push_back(line.into());
        while self.log.len() > MAX_LOG_LINES {
            self.log.pop_front();
        }
    }

    fn invalidate_plan(&mut self) {
        self.plan = None;
        self.confirm_open = false;
        self.focus_preview = false;
        self.confirmation.clear();
        self.error = None;
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
            Message::ImagePath(value) if self.idle() && !self.confirm_open && !self.show_log => {
                self.image_path = value;
                self.image = None;
                self.invalidate_plan();
            }
            Message::BrowseImage if self.idle() && !self.confirm_open && !self.show_log => {
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
            Message::Inspect
                if self.idle()
                    && !self.confirm_open
                    && !self.show_log
                    && !self.image_path.trim().is_empty() =>
            {
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
            Message::ChooseDevice(value) if self.idle() && !self.confirm_open && !self.show_log => {
                self.selected = Some(value);
                self.invalidate_plan();
            }
            Message::TestMode(value) if self.idle() && !self.confirm_open && !self.show_log => {
                self.test_mode = value;
                self.selected = None;
                self.invalidate_plan();
            }
            Message::TestPath(value) if self.idle() && !self.confirm_open && !self.show_log => {
                self.test_path = value;
                self.invalidate_plan();
            }
            Message::MakePlan if self.can_plan() => {
                self.planning = true;
                self.invalidate_plan();
                self.status = "Building a read-only operation plan…".into();
                let source = PathBuf::from(self.image_path.trim());
                let target = self.target().expect("can_plan requires a target");
                let test_file = self.test_mode;
                let tx = self.tx.clone();
                std::thread::spawn(move || {
                    let result = forgeusb_linux::plan(&source, &target, test_file)
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
                self.status = "Operation cancelled before writing.".into();
            }
            Message::Start if self.can_start() => self.start(),
            Message::Cancel => {
                if let Some(running) = &self.running {
                    running.cancel.cancel();
                    self.status =
                        "Cancellation requested. Waiting for the current I/O boundary and flush…"
                            .into();
                    self.append_log(
                        "Cancellation requested; the target may contain an incomplete image.",
                    );
                } else if self.picking {
                    self.picker_cancel.cancel();
                } else if self.confirm_open {
                    return self.update(Message::DismissPlan);
                } else if self.show_log {
                    self.show_log = false;
                }
            }
            Message::ToggleTheme => self.dark = !self.dark,
            Message::ToggleLog if !self.confirm_open => self.show_log = !self.show_log,
            Message::ToggleAdvanced => self.advanced = !self.advanced,
            Message::ToggleAdvancedFormat => self.advanced_format = !self.advanced_format,
            Message::FocusImage => return self.update(Message::BrowseImage),
            Message::FocusNext(backward) => {
                return if backward {
                    iced::widget::focus_previous()
                } else {
                    iced::widget::focus_next()
                };
            }
            Message::SelectRelative(direction)
                if self.idle()
                    && !self.confirm_open
                    && !self.show_log
                    && !self.test_mode
                    && !self.choices.is_empty() =>
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
                self.selected = Some(self.choices[index].clone());
                self.invalidate_plan();
            }
            Message::ToggleTestMode if self.idle() && !self.confirm_open && !self.show_log => {
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
                        self.choices = devices
                            .iter()
                            .filter(|d| d.eligible)
                            .map(|d| DeviceChoice {
                                path: d.path.clone(),
                                label: format!(
                                    "{} ({}) [{}]",
                                    ellipsize(&d.model, 28),
                                    d.path.display(),
                                    size(d.capacity)
                                ),
                            })
                            .collect();
                        if let Some(selected) = &self.selected {
                            let new = devices.iter().find(|d| d.path == selected.path);
                            let old = self.devices.iter().find(|d| d.path == selected.path);
                            let unchanged = new.zip(old).is_some_and(|(new, old)| {
                                new.eligible
                                    && new.identity == old.identity
                                    && new.capacity == old.capacity
                            });
                            if !unchanged && self.running.is_none() {
                                self.selected = None;
                                self.invalidate_plan();
                                self.status =
                                    "Target changed or disconnected. Select and preview it again."
                                        .into();
                                self.append_log("Target selection cleared because device eligibility or identity changed.");
                            }
                        }
                        self.devices = devices;
                    }
                    Err(error) => {
                        self.devices.clear();
                        self.choices.clear();
                        if self.running.is_none() && !self.test_mode {
                            self.selected = None;
                            self.invalidate_plan();
                        }
                        if self.error.as_ref() != Some(&error) {
                            self.append_log(format!("Device discovery failed: {error}"));
                        }
                        self.error = Some(error);
                    }
                }
            }
            WorkerEvent::Inspected(result) => {
                self.inspecting = false;
                match result {
                    Ok(image) => {
                        self.status = "Image analyzed. Press START to review the operation.".into();
                        self.append_log(format!(
                            "Analyzed {} ({}); SHA-256 {}",
                            image.path.display(),
                            size(image.size),
                            image.sha256
                        ));
                        self.image = Some(image);
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
                        self.image = Some(plan.image.clone());
                        self.append_log(format!(
                            "Read-only preview: {} -> {} ({} bytes)",
                            plan.image.path.display(),
                            plan.target.display(),
                            plan.image.size
                        ));
                        self.status =
                            "Review the operation and confirm the selected target.".into();
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
                match result {
                    Ok(summary) => {
                        self.status = summary.clone();
                        self.append_log(summary);
                    }
                    Err(error) => self.fail(error),
                }
                self.refresh();
            }
        }
    }

    fn fail(&mut self, error: String) {
        self.status = "Operation stopped. Review the error below.".into();
        self.append_log(format!("Error: {error}"));
        self.error = Some(error);
    }

    fn idle(&self) -> bool {
        self.running.is_none() && !self.inspecting && !self.planning && !self.picking
    }

    fn target(&self) -> Option<PathBuf> {
        if self.test_mode {
            (!self.test_path.trim().is_empty()).then(|| PathBuf::from(self.test_path.trim()))
        } else {
            self.selected.as_ref().map(|d| d.path.clone())
        }
    }

    fn can_plan(&self) -> bool {
        self.idle()
            && !self.confirm_open
            && !self.show_log
            && !self.image_path.trim().is_empty()
            && self.image.as_ref().is_none_or(|image| image.bootable_disk)
            && self.target().is_some()
    }

    fn can_start(&self) -> bool {
        self.idle()
            && self.confirm_open
            && !self.show_log
            && self.smoke_started.is_none()
            && self
                .plan
                .as_ref()
                .is_some_and(|p| self.confirmation == p.confirmation)
    }

    fn start(&mut self) {
        let plan = self.plan.clone().expect("can_start requires a plan");
        let confirmation = self.confirmation.clone();
        self.confirm_open = false;
        let cancel = Cancellation::default();
        self.running = Some(Running {
            cancel: cancel.clone(),
            progress: Progress {
                stage: Stage::Revalidating,
                completed: 0,
                total: plan.image.size,
                message: "Revalidating source and target…".into(),
            },
            started: Instant::now(),
        });
        self.error = None;
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
                        Ok(format!(
                            "Verified {} bytes · SHA-256 {}",
                            report.bytes, report.sha256
                        ))
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
                    success: Color::from_rgb8(6, 176, 37),
                    danger: Color::from_rgb8(165, 28, 28),
                }
            },
        )
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            iced::time::every(Duration::from_millis(100)).map(|_| Message::Tick),
            iced::window::close_requests().map(|_| Message::CloseRequested),
            iced::event::listen_with(|event, _, _| {
                if let iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
                    key,
                    modifiers,
                    ..
                }) = event
                {
                    shortcut(key, modifiers)
                } else {
                    None
                }
            }),
        ])
    }

    fn view(&self) -> Element<'_, Message> {
        let editable = self.idle() && !self.confirm_open && !self.show_log;
        let mut drive = column![section_title("Drive Properties")].spacing(6);
        let target: Element<'_, Message> = if self.test_mode {
            let input = text_input("New output file (must not exist)", &self.test_path)
                .size(12)
                .padding([2, 5])
                .style(classic_input)
                .on_submit(Message::MakePlan);
            if editable {
                input.on_input(Message::TestPath).into()
            } else {
                input.into()
            }
        } else if editable {
            pick_list(
                self.choices.clone(),
                self.selected.clone(),
                Message::ChooseDevice,
            )
            .placeholder(if self.choices.is_empty() {
                "No USB devices found"
            } else {
                "Select a USB device"
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
                    .unwrap_or_else(|| "No USB device selected".into()),
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
        let source = text_input("Disk or ISO image (please select)", &self.image_path)
            .id(text_input::Id::new("source-image"))
            .size(12)
            .padding([2, 5])
            .style(classic_input)
            .on_submit(Message::Inspect);
        let source = if editable {
            source.on_input(Message::ImagePath)
        } else {
            source
        };
        drive = drive.push(field(
            "Boot selection",
            row![
                source,
                button(text("SELECT").size(12).center())
                    .width(96)
                    .padding([2, 8])
                    .style(classic_button)
                    .on_press_maybe(editable.then_some(Message::BrowseImage)),
            ]
            .spacing(8)
            .align_y(iced::Center),
        ));
        drive = drive.push(field(
            "Image option",
            readonly(
                if self
                    .image
                    .as_ref()
                    .is_some_and(|image| !image.bootable_disk)
                {
                    "Unsupported image for DD mode"
                } else {
                    "Write in DD Image mode"
                },
            ),
        ));
        drive = drive.push(
            row![
                field("Partition scheme", readonly("From image")),
                field("Target system", readonly("From image")),
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
            let file_mode = checkbox("Use a disposable image file", self.test_mode)
                .size(14)
                .text_size(12);
            drive = drive.push(if editable {
                file_mode.on_toggle(Message::TestMode)
            } else {
                file_mode
            });
            if let Some(image) = &self.image {
                drive = drive
                    .push(text(format!("{:?} image, {}", image.kind, size(image.size))).size(11));
                drive = drive.push(
                    text(format!("SHA-256: {}", image.sha256))
                        .size(10)
                        .font(Font::MONOSPACE),
                );
                for warning in &image.warnings {
                    drive = drive.push(text(warning).size(11).style(text::danger));
                }
            }
            if let Some(device) = self.selected.as_ref().and_then(|selected| {
                self.devices
                    .iter()
                    .find(|device| device.path == selected.path)
            }) {
                drive = drive.push(
                    text(format!(
                        "{} {} — {} bytes\n{}:{}; serial {}",
                        device.vendor,
                        device.model,
                        device.capacity,
                        device.identity.major,
                        device.identity.minor,
                        device.identity.serial.as_deref().unwrap_or("unavailable")
                    ))
                    .size(11),
                );
                for partition in &device.partitions {
                    drive = drive.push(
                        text(format!(
                            "{}: {}, {}",
                            partition.path.display(),
                            partition
                                .filesystem
                                .as_deref()
                                .unwrap_or("unknown filesystem"),
                            if partition.mounts.is_empty() {
                                "unmounted".into()
                            } else {
                                partition.mounts.join(", ")
                            }
                        ))
                        .size(11),
                    );
                }
            }
            for device in self.devices.iter().filter(|device| !device.eligible) {
                drive = drive.push(
                    text(format!(
                        "{}: {}",
                        device.path.display(),
                        device.reasons.join("; ")
                    ))
                    .size(11)
                    .style(text::secondary),
                );
            }
        }

        let mut format = column![
            section_title("Format Options"),
            field("Volume label", readonly("Preserve image label")),
            row![
                field("File system", readonly("From image")),
                field("Cluster size", readonly("From image"))
            ]
            .spacing(36),
            button(
                text(if self.advanced_format {
                    "Hide advanced format options"
                } else {
                    "Show advanced format options"
                })
                .size(12)
            )
            .padding([1, 0])
            .style(button::text)
            .on_press_maybe(
                (!self.confirm_open && !self.show_log).then_some(Message::ToggleAdvancedFormat)
            ),
        ]
        .spacing(6);
        if self.advanced_format {
            format = format.push(
                checkbox("Verify written image (required)", true)
                    .size(14)
                    .text_size(12),
            );
            format = format.push(text("DD mode preserves the image's partitions, filesystem, label and bootloader. Read-back checks the source image range; unused target space is outside verification coverage.").size(11));
            format = format.push(
                checkbox("Dark theme", self.dark)
                    .size(14)
                    .text_size(12)
                    .on_toggle(|_| Message::ToggleTheme),
            );
        }
        let mut content = column![drive, format].spacing(14);
        if self
            .image
            .as_ref()
            .is_some_and(|image| !image.bootable_disk)
        {
            content = content.push(text("This image cannot be written in DD mode. Select an ISOHybrid or a disk image. ISO extraction is not available yet.").size(12).style(text::danger));
        }
        if let Some(error) = &self.error {
            content = content.push(
                text(error.chars().take(600).collect::<String>())
                    .size(12)
                    .style(text::danger),
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
            .unwrap_or(if self.status.starts_with("Verified ") {
                100.0
            } else {
                0.0
            });
        let bar_label = if let Some(running) = &self.running {
            if running.cancel.is_cancelled() {
                "CANCELLING".into()
            } else {
                format!("{}: {:.1}%", stage_name(running.progress.stage), percent)
            }
        } else if self.picking {
            "SELECT AN IMAGE".into()
        } else if self.inspecting {
            "ANALYZING IMAGE".into()
        } else if self.planning {
            "PREPARING OPERATION".into()
        } else if self.error.is_some() {
            "ERROR".into()
        } else if self.status.starts_with("Verified ") {
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
        let bottom = if self.running.is_some()
            || self.planning
            || self.inspecting
            || self.error.is_some()
            || self.status.starts_with("Verified ")
        {
            ellipsize(&self.status, 58)
        } else if self.test_mode {
            "Disposable file mode — new files only".into()
        } else {
            format!("{} USB device(s) found", self.choices.len())
        };
        let elapsed = self
            .running
            .as_ref()
            .map(|running| {
                let seconds = running.started.elapsed().as_secs();
                format!(
                    "{:02}:{:02}:{:02}",
                    seconds / 3600,
                    seconds / 60 % 60,
                    seconds % 60
                )
            })
            .unwrap_or_default();
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
            container(row![text(bottom).size(11).width(Fill), text(elapsed).size(11)].spacing(8))
                .padding([5, 7]),
        ])
        .width(Fill)
        .height(Fill)
        .style(dialog_background)
        .into();
        if self.confirm_open && self.plan.is_some() {
            modal(base, self.confirmation_view())
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

    fn confirmation_view(&self) -> Element<'_, Message> {
        let plan = self.plan.as_ref().expect("confirmation requires a plan");
        let physical = matches!(plan.target_kind, TargetKind::Block(_));
        let mut details = column![
            text(if physical {
                "All data on this device will be destroyed."
            } else {
                "Create a new disposable image file."
            })
            .size(13),
            text(format!(
                "Image: {}\nTarget: {}\nImage size: {} bytes\nCapacity: {} bytes",
                plan.image.path.display(),
                plan.target.display(),
                plan.image.size,
                plan.capacity
            ))
            .size(12),
            text("Write image, flush, then verify every source byte by read-back.").size(12),
        ]
        .spacing(8);
        for warning in &plan.warnings {
            details = details.push(text(warning).size(11));
        }
        let confirm = text_input("Type the exact confirmation above", &self.confirmation)
            .id(text_input::Id::new("confirmation"))
            .size(12)
            .padding([5, 6])
            .style(classic_input)
            .on_input(Message::Confirmation)
            .on_submit(Message::Start);
        column![
            text("Confirm operation").size(18),
            scrollable(details).height(190),
            horizontal_rule(1),
            text("To confirm the model, path and capacity, type:").size(12),
            text(&plan.confirmation).size(12).font(Font::MONOSPACE),
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

fn field<'a>(label: &'a str, control: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    column![text(label).size(12), control.into()]
        .spacing(3)
        .width(Fill)
        .into()
}

fn readonly<'a>(value: impl Into<String>) -> Element<'a, Message> {
    let value = value.into();
    text_input("", &value)
        .size(12)
        .padding([2, 5])
        .style(classic_input)
        .into()
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
        .args(["write", "--plan-json"])
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
                        complete = Some(format!(
                            "Verified {} bytes · SHA-256 {}",
                            report.bytes, report.sha256
                        ));
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
        state.confirmation = format!("{} ", plan.confirmation);
        state.plan = Some(plan.clone());
        state.confirm_open = true;
        assert!(!state.can_start());
        state.confirmation = plan.confirmation;
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
        let plan = plan();
        state.confirmation = plan.confirmation.clone();
        state.plan = Some(plan);
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
}
