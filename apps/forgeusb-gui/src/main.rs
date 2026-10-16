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
    iced::application("ForgeUSB · bootable media", State::update, State::view)
        .theme(State::theme)
        .subscription(State::subscription)
        .window_size((980.0, 900.0))
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
    Inspect,
    ChooseDevice(DeviceChoice),
    TestMode(bool),
    TestPath(String),
    MakePlan,
    Confirmation(String),
    Start,
    Cancel,
    ToggleTheme,
    ToggleLog,
    ToggleAdvanced,
    FocusImage,
    FocusNext(bool),
    SelectRelative(i8),
    ToggleTestMode,
    CloseRequested,
}

enum WorkerEvent {
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
            dark: true,
            show_log: false,
            advanced: false,
            error: None,
            status: "Choose an image and a target to begin.".into(),
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
                    return Task::batch([
                        text_input::focus(text_input::Id::new("confirmation")),
                        scrollable::snap_to(
                            scrollable::Id::new("workflow"),
                            scrollable::RelativeOffset::END,
                        ),
                    ]);
                }
                self.ticks = self.ticks.wrapping_add(1);
                if self.ticks.is_multiple_of(30) {
                    self.refresh();
                }
                if self.exit_after_operation && self.running.is_none() {
                    return iced::exit();
                }
                if self
                    .smoke_started
                    .is_some_and(|start| start.elapsed() >= Duration::from_secs(10))
                {
                    println!(
                        "ForgeUSB GUI smoke test: event loop ran, {} storage devices discovered; no write requested.",
                        self.devices.len()
                    );
                    return iced::exit();
                }
            }
            Message::Refresh => self.refresh(),
            Message::ImagePath(value) if self.idle() => {
                self.image_path = value;
                self.image = None;
                self.invalidate_plan();
            }
            Message::Inspect if self.idle() && !self.image_path.trim().is_empty() => {
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
            Message::ChooseDevice(value) if self.idle() => {
                self.selected = Some(value);
                self.invalidate_plan();
            }
            Message::TestMode(value) if self.idle() => {
                self.test_mode = value;
                self.selected = None;
                self.invalidate_plan();
            }
            Message::TestPath(value) if self.idle() => {
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
                }
            }
            Message::ToggleTheme => self.dark = !self.dark,
            Message::ToggleLog => self.show_log = !self.show_log,
            Message::ToggleAdvanced => self.advanced = !self.advanced,
            Message::FocusImage => return text_input::focus(text_input::Id::new("source-image")),
            Message::FocusNext(backward) => {
                return if backward {
                    iced::widget::focus_previous()
                } else {
                    iced::widget::focus_next()
                };
            }
            Message::SelectRelative(direction)
                if self.idle() && !self.test_mode && !self.choices.is_empty() =>
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
            Message::ToggleTestMode if self.idle() => {
                return self.update(Message::TestMode(!self.test_mode));
            }
            Message::CloseRequested => {
                if let Some(running) = &self.running {
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
                                    "{} · {} · {}",
                                    d.path.display(),
                                    d.model,
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
                        self.status =
                            "Image analyzed. Preview the operation before writing.".into();
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
                            "Review the exact plan, then type the confirmation below.".into();
                        self.plan = Some(*plan);
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
        self.running.is_none() && !self.inspecting && !self.planning
    }

    fn target(&self) -> Option<PathBuf> {
        if self.test_mode {
            (!self.test_path.trim().is_empty()).then(|| PathBuf::from(self.test_path.trim()))
        } else {
            self.selected.as_ref().map(|d| d.path.clone())
        }
    }

    fn can_plan(&self) -> bool {
        self.idle() && !self.image_path.trim().is_empty() && self.target().is_some()
    }

    fn can_start(&self) -> bool {
        self.idle()
            && self.smoke_started.is_none()
            && self
                .plan
                .as_ref()
                .is_some_and(|p| self.confirmation == p.confirmation)
    }

    fn start(&mut self) {
        let plan = self.plan.clone().expect("can_start requires a plan");
        let confirmation = self.confirmation.clone();
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
            "ForgeUSB".into(),
            if self.dark {
                iced::theme::Palette {
                    background: Color::from_rgb8(18, 22, 29),
                    text: Color::from_rgb8(231, 235, 241),
                    primary: Color::from_rgb8(100, 219, 184),
                    success: Color::from_rgb8(100, 219, 184),
                    danger: Color::from_rgb8(255, 127, 117),
                }
            } else {
                iced::theme::Palette {
                    background: Color::from_rgb8(242, 245, 248),
                    text: Color::from_rgb8(28, 39, 47),
                    primary: Color::from_rgb8(15, 115, 94),
                    success: Color::from_rgb8(15, 115, 94),
                    danger: Color::from_rgb8(173, 45, 42),
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
        let header = row![
            column![
                text("FORGEUSB").size(30),
                text("Bootable media, with every step in view.")
                    .size(14)
                    .style(text::secondary),
            ]
            .spacing(4)
            .width(Fill),
            button(if self.dark {
                "Light theme"
            } else {
                "Dark theme"
            })
            .on_press(Message::ToggleTheme)
            .style(button::secondary),
        ]
        .align_y(iced::Center);

        let source_input = text_input("/home/you/Downloads/image.iso", &self.image_path)
            .id(text_input::Id::new("source-image"))
            .padding(12)
            .size(15)
            .on_submit(Message::Inspect);
        let source_input = if self.idle() {
            source_input.on_input(Message::ImagePath)
        } else {
            source_input
        };
        let mut source = column![
            section_title("01", "Source image"),
            row![
                source_input,
                button(if self.inspecting {
                    "Analyzing…"
                } else {
                    "Analyze image"
                })
                .padding(12)
                .on_press_maybe(
                    (self.idle() && !self.image_path.trim().is_empty()).then_some(Message::Inspect)
                ),
            ]
            .spacing(10),
        ]
        .spacing(12);
        if let Some(image) = &self.image {
            source = source.push(
                row![
                    badge(format!("{:?}", image.kind)),
                    badge(size(image.size)),
                    badge(if image.bootable_disk {
                        "Disk layout detected"
                    } else {
                        "Boot compatibility requires review"
                    }),
                ]
                .spacing(10),
            );
            source = source.push(
                text(format!("SHA-256  {}", image.sha256))
                    .font(Font::MONOSPACE)
                    .size(11),
            );
            for warning in &image.warnings {
                source = source.push(text(warning).size(13).style(text::danger));
            }
        } else {
            source = source.push(
                text("ISO, hybrid ISO and raw disk images · structural inspection + SHA-256")
                    .size(13)
                    .style(text::secondary),
            );
        }

        let mut target = column![
            row![
                section_title("02", "Destination").width(Fill),
                button(if self.discovering {
                    "Refreshing…"
                } else {
                    "Refresh devices"
                })
                .on_press_maybe((!self.discovering).then_some(Message::Refresh))
                .style(button::secondary),
            ]
            .align_y(iced::Center),
        ]
        .spacing(12);
        if self.test_mode {
            let input = text_input("/home/you/usb-test.img (must not exist)", &self.test_path)
                .padding(12)
                .on_submit(Message::MakePlan);
            target = target.push(if self.idle() {
                input.on_input(Message::TestPath)
            } else {
                input
            });
            target = target.push(
                text("Disposable file mode creates a new file. Existing paths are refused.")
                    .size(13)
                    .style(text::secondary),
            );
        } else {
            let selector = pick_list(
                self.choices.clone(),
                self.selected.clone(),
                Message::ChooseDevice,
            )
            .placeholder("Select a USB device — nothing is selected automatically")
            .padding(12)
            .width(Fill);
            target = if self.idle() {
                target.push(selector)
            } else {
                target.push(
                    text(
                        self.selected
                            .as_ref()
                            .map(ToString::to_string)
                            .unwrap_or_else(|| "No device selected".into()),
                    )
                    .size(14),
                )
            };
            if self.choices.is_empty() {
                target = target.push(text("No eligible USB devices found. Connect an unmounted USB drive or use disposable file mode.").size(13).style(text::secondary));
            }
            for device in self.devices.iter().filter(|d| !d.eligible) {
                target = target.push(
                    text(format!(
                        "Protected · {} · {} · {}",
                        device.path.display(),
                        device.model,
                        device.reasons.join("; ")
                    ))
                    .size(12)
                    .style(text::secondary),
                );
            }
            if let Some(device) = self
                .selected
                .as_ref()
                .and_then(|s| self.devices.iter().find(|d| d.path == s.path))
            {
                target = target.push(
                    text(format!(
                        "{} {} · {} bytes · {}:{} · serial {}",
                        device.vendor,
                        device.model,
                        device.capacity,
                        device.identity.major,
                        device.identity.minor,
                        device.identity.serial.as_deref().unwrap_or("unavailable")
                    ))
                    .size(12),
                );
                for partition in &device.partitions {
                    target = target.push(
                        text(format!(
                            "{} · {} · {} · {}",
                            partition.path.display(),
                            size(partition.capacity),
                            partition
                                .filesystem
                                .as_deref()
                                .unwrap_or("filesystem unknown"),
                            if partition.mounts.is_empty() {
                                "unmounted".into()
                            } else {
                                partition.mounts.join(", ")
                            }
                        ))
                        .size(12)
                        .style(text::secondary),
                    );
                }
            }
        }
        let test_checkbox = checkbox(
            "Use a new disposable image file instead of a USB drive",
            self.test_mode,
        )
        .size(16)
        .text_size(13);
        target = target.push(if self.idle() {
            test_checkbox.on_toggle(Message::TestMode)
        } else {
            test_checkbox
        });

        let mut workflow = column![
            section_title("03", "Writing method"),
            row![
                column![
                    text("PARTITION SCHEME").size(10).style(text::secondary),
                    text("Preserve image layout").size(14)
                ]
                .spacing(5)
                .width(Fill),
                column![
                    text("TARGET FIRMWARE").size(10).style(text::secondary),
                    text("From image").size(14)
                ]
                .spacing(5)
                .width(Fill),
                column![
                    text("FILESYSTEM").size(10).style(text::secondary),
                    text("From image").size(14)
                ]
                .spacing(5)
                .width(Fill),
                column![
                    text("VOLUME LABEL").size(10).style(text::secondary),
                    text("Preserve image label").size(14)
                ]
                .spacing(5)
                .width(Fill),
            ]
            .spacing(12),
            button(if self.advanced {
                "Hide advanced details"
            } else {
                "Advanced details"
            })
            .on_press(Message::ToggleAdvanced)
            .style(button::text),
        ]
        .spacing(12);
        if self.advanced {
            workflow = workflow.push(text("Raw mode copies the source bytes without changing its partition table, volume label, filesystem or bootloader. Read-back verifies the written image range; it does not prove firmware boot compatibility. The unused end of a larger target is outside verification coverage.").size(13));
            workflow = workflow.push(text("Physical writes use the installed Polkit helper. Mounted or active system storage is rejected by the shared safety engine. Unmount eligible USB partitions through your desktop before planning.").size(13).style(text::secondary));
            workflow = workflow.push(text("Keyboard: Ctrl+O source · Alt+Up/Down device · Alt+F file mode · Ctrl+P preview · F8 start · Esc cancel · Alt+T theme · Alt+L log. Tab/Shift+Tab move between text fields.").size(12).style(text::secondary));
        }
        workflow = workflow.push(
            button(if self.planning {
                "Preparing preview…"
            } else {
                "Preview exact operation"
            })
            .padding(12)
            .on_press_maybe(self.can_plan().then_some(Message::MakePlan)),
        );

        let mut content = column![header, card(source), card(target), card(workflow)].spacing(18);
        if let Some(plan) = &self.plan {
            let physical = matches!(&plan.target_kind, TargetKind::Block(_));
            let mut preview = column![
                text(if physical {
                    "ALL EXISTING DATA ON THIS DEVICE WILL BE DESTROYED"
                } else {
                    "CREATE A NEW DISPOSABLE OUTPUT FILE"
                })
                .size(15)
                .style(if physical {
                    text::danger
                } else {
                    text::success
                }),
                text(format!(
                    "{} to {}",
                    plan.image.path.display(),
                    plan.target.display()
                ))
                .size(14),
                text(format!(
                    "{} bytes · target capacity {} bytes",
                    plan.image.size, plan.capacity
                ))
                .size(13),
                text(format!("Strategy: {}", plan.strategy)).size(13),
                text("Revalidate identity, write image, flush, read back and compare SHA-256")
                    .size(13),
            ]
            .spacing(10);
            for warning in &plan.warnings {
                preview = preview.push(text(warning).size(13).style(text::danger));
            }
            preview = preview.push(horizontal_rule(1));
            preview = preview.push(
                text(format!("Type exactly: {}", plan.confirmation))
                    .size(13)
                    .font(Font::MONOSPACE),
            );
            let confirm = text_input("Explicit confirmation", &self.confirmation)
                .id(text_input::Id::new("confirmation"))
                .padding(12)
                .on_submit(Message::Start);
            preview = preview.push(if self.running.is_none() {
                confirm.on_input(Message::Confirmation)
            } else {
                confirm
            });
            preview = preview.push(
                button(if physical {
                    "Erase device, write & verify"
                } else {
                    "Create file, write & verify"
                })
                .padding(12)
                .on_press_maybe(self.can_start().then_some(Message::Start))
                .style(if physical {
                    button::danger
                } else {
                    button::primary
                }),
            );
            content = content.push(card(preview));
        }
        let mut status = column![text(&self.status).size(14)].spacing(10);
        if let Some(running) = &self.running {
            let progress = &running.progress;
            let percent = if progress.total == 0 {
                0.0
            } else {
                (progress.completed as f64 / progress.total as f64 * 100.0).clamp(0.0, 100.0) as f32
            };
            status = status.push(progress_bar(0.0..=100.0, percent));
            status = status.push(
                row![
                    text(format!(
                        "{:?} · {:.1}% · {} / {} · {}s",
                        progress.stage,
                        percent,
                        size(progress.completed),
                        size(progress.total),
                        running.started.elapsed().as_secs()
                    ))
                    .size(12)
                    .width(Fill),
                    button(if running.cancel.is_cancelled() {
                        "Cancelling…"
                    } else {
                        "Cancel safely"
                    })
                    .on_press_maybe((!running.cancel.is_cancelled()).then_some(Message::Cancel))
                    .style(button::danger),
                ]
                .align_y(iced::Center),
            );
            status = status.push(text("Interruption can leave this target incomplete. Keep it connected until the operation stops.").size(12).style(text::danger));
        }
        if let Some(error) = &self.error {
            let concise: String = error.chars().take(600).collect();
            status = status.push(text(concise).style(text::danger).size(13));
        }
        status = status.push(
            button(if self.show_log {
                "Hide execution log"
            } else {
                "Show execution log"
            })
            .on_press(Message::ToggleLog)
            .style(button::text),
        );
        if self.show_log {
            let lines = self.log.iter().fold(column![].spacing(6), |column, line| {
                column.push(text(line).font(Font::MONOSPACE).size(11))
            });
            status = status.push(scrollable(lines).height(180));
        }
        let footer = column![
            card(status),
            text("ForgeUSB 0.1 · Linux native · Raw image engine · No target is selected automatically")
                .size(11)
                .style(text::secondary),
        ].spacing(10);
        container(column![
            scrollable(container(content).max_width(1060).padding(28))
                .id(scrollable::Id::new("workflow"))
                .width(Fill)
                .height(Fill),
            container(footer).max_width(1060).padding([12, 28]),
        ])
        .style(|theme: &Theme| container::Style {
            background: Some(theme.palette().background.into()),
            text_color: Some(theme.palette().text),
            ..Default::default()
        })
        .width(Fill)
        .height(Fill)
        .into()
    }
}

fn section_title<'a>(number: &'a str, title: &'a str) -> iced::widget::Row<'a, Message> {
    row![
        text(number).size(13).style(text::success),
        text(title).size(19)
    ]
    .spacing(12)
    .align_y(iced::Center)
}

fn shortcut(key: iced::keyboard::Key, modifiers: iced::keyboard::Modifiers) -> Option<Message> {
    use iced::keyboard::{Key, key::Named};
    match key.as_ref() {
        Key::Named(Named::Tab) => Some(Message::FocusNext(modifiers.shift())),
        Key::Named(Named::Escape) => Some(Message::Cancel),
        Key::Named(Named::F8) => Some(Message::Start),
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

fn card<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    container(content)
        .padding(20)
        .width(Fill)
        .style(|theme: &Theme| {
            let dark = theme.palette().background.r < 0.5;
            container::Style {
                background: Some(
                    if dark {
                        Color::from_rgb8(26, 33, 43)
                    } else {
                        Color::WHITE
                    }
                    .into(),
                ),
                text_color: Some(theme.palette().text),
                border: iced::Border {
                    color: if dark {
                        Color::from_rgb8(48, 61, 76)
                    } else {
                        Color::from_rgb8(210, 219, 228)
                    },
                    width: 1.0,
                    radius: 10.0.into(),
                },
                ..Default::default()
            }
        })
        .into()
}

fn badge<'a>(label: impl Into<String>) -> Element<'a, Message> {
    container(text(label.into()).size(12))
        .padding([5, 9])
        .style(|theme: &Theme| container::Style {
            text_color: Some(theme.palette().text),
            border: iced::Border {
                color: theme.palette().primary.scale_alpha(0.45),
                width: 1.0,
                radius: 5.0.into(),
            },
            ..Default::default()
        })
        .into()
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
}
