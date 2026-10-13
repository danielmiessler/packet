//! The menu-bar app: verdict icon, layer-by-layer menu, Test Now, and the Packet window (current
//! state plus history), which opens on launch and whenever the app is opened again.

use crate::login::{self, LoginState};
use base64::Engine;
use sha2::{Digest, Sha256};
use crate::probe::REGIONS;
use crate::sampler::{self, Cmd, Snapshot};
use crate::verdict::Level;
use crate::{history, report, sched, store, time};
use std::borrow::Cow;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget};
use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS, EventLoopWindowTargetExtMacOS, WindowBuilderExtMacOS, WindowExtMacOS};
use tao::window::{Window, WindowBuilder};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
use wry::http::{header::CONTENT_TYPE, Response};
use wry::{WebView, WebViewBuilder};

const HISTORY_PAGE: &str = include_str!("history.html");
const HISTORY_URL: &str = "packet://localhost/history";
const LOGIN_SETTINGS: &str = "x-apple.systempreferences:com.apple.LoginItems-Settings.extension";
const MAX_IPC_BYTES: usize = 256;
/// The "4 min ago" texts age even when no round arrives.
const TICK: Duration = Duration::from_secs(30);

const GLYPH_GOOD: &[u8] = include_bytes!("../bundle/glyph-good.png");
const GLYPH_FAIR: &[u8] = include_bytes!("../bundle/glyph-fair.png");
const GLYPH_POOR: &[u8] = include_bytes!("../bundle/glyph-poor.png");
const GLYPH_OFFLINE: &[u8] = include_bytes!("../bundle/glyph-offline.png");

enum UserEvent {
    Menu(MenuEvent),
    Snap(Box<Snapshot>),
    Ipc(String),
    History(&'static str, String),
}

pub fn glyph_bytes(level: Option<Level>) -> &'static [u8] {
    match level {
        Some(Level::Good) => GLYPH_GOOD,
        Some(Level::Fair) => GLYPH_FAIR,
        Some(Level::Poor) => GLYPH_POOR,
        Some(Level::Offline) | None => GLYPH_OFFLINE,
    }
}

fn decode_png(bytes: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(bytes));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::ALPHA);
    let mut reader = dec.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    buf.truncate(info.buffer_size());
    match info.color_type {
        png::ColorType::Rgba => Some((buf, info.width, info.height)),
        _ => None,
    }
}

fn icon_for(level: Option<Level>) -> Option<Icon> {
    let (rgba, w, h) = decode_png(glyph_bytes(level))?;
    Icon::from_rgba(rgba, w, h).ok()
}

struct Items {
    verdict: MenuItem,
    link: MenuItem,
    edge: MenuItem,
    dns: MenuItem,
    world: Submenu,
    regions: Vec<MenuItem>,
    bandwidth: MenuItem,
    quick: MenuItem,
    measured: MenuItem,
    test: MenuItem,
    history: MenuItem,
    hourly: CheckMenuItem,
    login: CheckMenuItem,
    quit: MenuItem,
}

fn build_menu() -> (Menu, Items) {
    let row = |t: &str| MenuItem::new(t, false, None);
    let regions: Vec<MenuItem> = REGIONS.iter().map(|r| row(r.name)).collect();
    let world = Submenu::new("World · measuring…", true);
    for r in &regions {
        world.append(r).expect("region row");
    }
    let items = Items {
        verdict: row("Measuring the first round…"),
        link: row("Link · measuring…"),
        edge: row("Internet · measuring…"),
        dns: row("DNS · measuring…"),
        world,
        regions,
        bandwidth: row("Line speed · not measured yet: use Test Now"),
        quick: row("Quick check · every 10 minutes"),
        measured: row(""),
        test: MenuItem::new("Test Now (full capacity, about 15 s)", true, None),
        history: MenuItem::new("Open Packet…", true, None),
        hourly: CheckMenuItem::new("Hourly line-speed test (about 3 GB a day)", true, sched::hourly_enabled(), None),
        login: CheckMenuItem::new("Open at login", true, false, None),
        quit: MenuItem::new("Quit Packet", true, None),
    };
    let menu = Menu::new();
    menu.append_items(&[
        &items.verdict,
        &PredefinedMenuItem::separator(),
        &items.link,
        &items.edge,
        &items.dns,
        &items.world,
        &items.bandwidth,
        &items.quick,
        &items.measured,
        &PredefinedMenuItem::separator(),
        &items.test,
        &items.history,
        &PredefinedMenuItem::separator(),
        &items.hourly,
        &items.login,
        &items.quit,
    ])
    .expect("build menu");
    (menu, items)
}

/// Tooltip lead: the nearest-edge latency, or the state when there is no number.
pub fn title_for(s: &Snapshot) -> String {
    match (s.verdict.level, s.round.edge.ms) {
        (Level::Offline, _) => "Offline".into(),
        (_, Some(e)) => format!("{e:.0} ms"),
        (_, None) => "—".into(),
    }
}

fn show(tray: &TrayIcon, items: &Items, s: &Snapshot) {
    let _ = tray.set_icon(icon_for(Some(s.verdict.level)));
    // Icon only: the menu bar is often full, and macOS hides the newest item first when it is.
    let _ = tray.set_tooltip(Some(format!("Packet · {} · {}", title_for(s), s.verdict.sentence)));
    items.verdict.set_text(format!("{} {}", s.verdict.level.mark(), s.verdict.sentence));
    items.link.set_text(format!("{} {}", report::mark(&s.findings[0]), report::link_row(&s.round)));
    items.edge.set_text(format!("{} {}", report::mark(&s.findings[1]), report::edge_row(&s.round)));
    items.dns.set_text(format!("{} {}", report::mark(&s.findings[2]), report::dns_row(&s.round)));
    items.world.set_text(format!("{} {}", report::mark(&s.findings[3]), report::world_row(&s.round)));
    for (item, text) in items.regions.iter().zip(report::region_rows(&s.round)) {
        item.set_text(text);
    }
    show_ages(items, s);
}

fn show_test(items: &Items, busy: bool) {
    items.test.set_text(if busy { "Testing bandwidth…" } else { "Test Now (full capacity, about 15 s)" });
    items.test.set_enabled(!busy);
}

fn show_ages(items: &Items, s: &Snapshot) {
    let now = time::now_ms();
    items.bandwidth.set_text(report::line_speed_row(s.full.as_ref(), now));
    items.quick.set_text(report::quick_row(s.quick.as_ref(), s.quick_share, now));
    items.measured.set_text(format!("Measured {} · every minute", time::hhmm(s.round.at_ms)));
}

fn show_login(items: &Items) {
    let s = login::state();
    items.login.set_text(s.label());
    items.login.set_checked(s == LoginState::Enabled);
}

/// First launch turns open-at-login on; after that the menu toggle is the only thing that changes it.
fn first_launch_login() {
    let marker = store::data_dir().join("login-initialized");
    if marker.exists() {
        return;
    }
    let _ = login::set(true);
    if matches!(login::state(), LoginState::Enabled | LoginState::RequiresApproval) {
        let _ = std::fs::create_dir_all(store::data_dir());
        let _ = std::fs::write(&marker, b"");
    }
}

/// Only Packet's own page may load in the history window.
pub fn allow_navigation(url: &str) -> bool {
    url.starts_with("packet://")
}

/// The history page with its one inline script pinned by hash.
pub fn page_html() -> String {
    let start = HISTORY_PAGE.find("<script>").expect("page has a script") + "<script>".len();
    let end = HISTORY_PAGE.find("</script>").expect("script closes");
    let hash = sha256_b64(HISTORY_PAGE[start..end].as_bytes());
    HISTORY_PAGE.replace("__SCRIPT_HASH__", &format!("sha256-{hash}"))
}

#[derive(Debug, PartialEq)]
pub enum PageRequest {
    Range(&'static str),
    TestNow,
    /// A mouse-down on the page's title strip: the title bar is hidden, so the page asks to move the window.
    Drag,
}

/// A request from the page, rebuilt from trusted parts.
pub fn parse_page_request(body: &str) -> Option<PageRequest> {
    if body.len() > MAX_IPC_BYTES {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    match v.get("type")?.as_str()? {
        "test" => Some(PageRequest::TestNow),
        "drag" => Some(PageRequest::Drag),
        "range" => {
            let want = v.get("range")?.as_str()?;
            history::RANGES.iter().map(|(n, _)| *n).find(|n| *n == want).map(PageRequest::Range)
        }
        _ => None,
    }
}

/// The current state for the window's top panel: the same rows the menu shows.
pub fn now_json(s: &Snapshot, testing: bool) -> String {
    let now = time::now_ms();
    let row = |f: &crate::verdict::Finding, label: &str, text: String| {
        serde_json::json!({ "level": f.level.slug(), "label": label, "text": text })
    };
    serde_json::json!({
        "level": s.verdict.level.slug(),
        "sentence": s.verdict.sentence,
        "rows": [
            row(&s.findings[0], "Link", report::link_row(&s.round)),
            row(&s.findings[1], "Internet", report::edge_row(&s.round)),
            row(&s.findings[2], "DNS", report::dns_row(&s.round)),
            row(&s.findings[3], "World", report::world_row(&s.round)),
        ],
        "regions": report::region_rows(&s.round),
        "line": report::line_speed_row(s.full.as_ref(), now),
        "quick": report::quick_row(s.quick.as_ref(), s.quick_share, now),
        "measured": format!("Measured {} · every minute", time::hhmm(s.round.at_ms)),
        "testing": testing,
    })
    .to_string()
}

/// The window's state when measuring has stalled: the last numbers, but no verdict drawn from them.
pub fn stale_json(s: &Snapshot, since: &str) -> String {
    let mut v: serde_json::Value = serde_json::from_str(&now_json(s, false)).expect("now_json is JSON");
    v["level"] = "offline".into();
    v["sentence"] = format!("No measurement since {since}. Restarting the measurements.").into();
    v.to_string()
}

/// Seconds as `ps -o etime` prints them: [[dd-]hh:]mm:ss.
pub fn parse_etime(e: &str) -> Option<u64> {
    let e = e.trim();
    let (days, rest) = match e.split_once('-') {
        Some((d, r)) => (d.parse::<u64>().ok()?, r),
        None => (0, e),
    };
    let parts: Vec<u64> = rest.split(':').map(|p| p.parse().ok()).collect::<Option<_>>()?;
    let secs = match parts.as_slice() {
        [m, s] => m * 60 + s,
        [h, m, s] => h * 3600 + m * 60 + s,
        _ => return None,
    };
    Some(days * 86_400 + secs)
}

/// A launch within three minutes of logging in is the login item starting Packet in the
/// background; the window opens only when the user opens the app themselves.
fn launched_at_login() -> bool {
    let pid = std::process::Command::new("/usr/bin/pgrep").args(["-x", "loginwindow"]).output().ok();
    let pid = pid.map(|o| String::from_utf8_lossy(&o.stdout).lines().next().unwrap_or("").trim().to_string()).unwrap_or_default();
    if pid.is_empty() {
        return false;
    }
    std::process::Command::new("/bin/ps")
        .args(["-o", "etime=", "-p", &pid])
        .output()
        .ok()
        .and_then(|o| parse_etime(&String::from_utf8_lossy(&o.stdout)))
        .is_some_and(|s| s < 180)
}

/// A new page starts on 24 h, so the range the app builds for is reset with it.
fn open_window(target: &EventLoopWindowTarget<UserEvent>, proxy: &EventLoopProxy<UserEvent>, range: &mut &'static str) -> HistoryWindow {
    *range = "24h";
    let window = WindowBuilder::new()
        .with_title("Packet")
        .with_inner_size(tao::dpi::LogicalSize::new(1080.0, 900.0))
        .with_min_inner_size(tao::dpi::LogicalSize::new(600.0, 560.0))
        .with_transparent(true)
        .with_titlebar_transparent(true)
        .with_title_hidden(true)
        .with_fullsize_content_view(true)
        .build(target)
        .expect("create Packet window");
    let ipc_proxy = proxy.clone();
    let page = page_html();
    let view = WebViewBuilder::new()
        .with_incognito(true)
        .with_transparent(true)
        .with_custom_protocol("packet".into(), move |_id, _req| {
            Response::builder()
                .header(CONTENT_TYPE, "text/html; charset=utf-8")
                .body(Cow::Owned(page.clone().into_bytes()))
                .expect("static response")
        })
        .with_url(HISTORY_URL)
        .with_navigation_handler(|url| allow_navigation(&url))
        .with_new_window_req_handler(|_url, _features| wry::NewWindowResponse::Deny)
        .with_ipc_handler(move |req| {
            let body = req.body();
            if body.len() <= MAX_IPC_BYTES {
                let _ = ipc_proxy.send_event(UserEvent::Ipc(body.clone()));
            }
        })
        .build(&window)
        .expect("create Packet view");
    glass(&window);
    target.set_activation_policy_at_runtime(ActivationPolicy::Regular);
    window.set_focus();
    bring_to_front();
    HistoryWindow { window, view }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Rect { x: f64, y: f64, w: f64, h: f64 }
unsafe impl objc2::encode::Encode for Rect {
    const ENCODING: objc2::encode::Encoding = objc2::encode::Encoding::Struct(
        "CGRect",
        &[
            objc2::encode::Encoding::Struct("CGPoint", &[f64::ENCODING, f64::ENCODING]),
            objc2::encode::Encoding::Struct("CGSize", &[f64::ENCODING, f64::ENCODING]),
        ],
    );
}

/// The window's glass: a dark NSVisualEffectView behind the transparent webview blurs the desktop,
/// and the page's blue-black tint sits on top of it. The window is held to the dark appearance so
/// the traffic lights match.
fn glass(window: &Window) {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};
    const HUD_WINDOW: isize = 13; // NSVisualEffectMaterialHUDWindow
    const BEHIND_WINDOW: isize = 0; // NSVisualEffectBlendingModeBehindWindow
    const ALWAYS_ACTIVE: isize = 1; // NSVisualEffectStateActive: stays frosted when unfocused
    const FILL: usize = 2 | 16; // NSViewWidthSizable | NSViewHeightSizable
    const BELOW: isize = -1; // NSWindowBelow
    let (Some(effect_cls), Some(appearance_cls), Some(string_cls)) =
        (AnyClass::get(c"NSVisualEffectView"), AnyClass::get(c"NSAppearance"), AnyClass::get(c"NSString"))
    else {
        return;
    };
    let view = window.ns_view() as *mut AnyObject;
    let ns_window = window.ns_window() as *mut AnyObject;
    if view.is_null() || ns_window.is_null() {
        return;
    }
    unsafe {
        let bounds: Rect = msg_send![view, bounds];
        let effect: *mut AnyObject = msg_send![effect_cls, alloc];
        let effect: *mut AnyObject = msg_send![effect, initWithFrame: bounds];
        if effect.is_null() {
            return;
        }
        let _: () = msg_send![effect, setMaterial: HUD_WINDOW];
        let _: () = msg_send![effect, setBlendingMode: BEHIND_WINDOW];
        let _: () = msg_send![effect, setState: ALWAYS_ACTIVE];
        let _: () = msg_send![effect, setAutoresizingMask: FILL];
        let nil: *mut AnyObject = std::ptr::null_mut();
        let _: () = msg_send![view, addSubview: effect, positioned: BELOW, relativeTo: nil];
        let name: *mut AnyObject = msg_send![string_cls, stringWithUTF8String: c"NSAppearanceNameDarkAqua".as_ptr()];
        let dark: *mut AnyObject = msg_send![appearance_cls, appearanceNamed: name];
        if !dark.is_null() {
            let _: () = msg_send![ns_window, setAppearance: dark];
        }
    }
}

/// Focusing the window is not enough for an app that runs without a Dock icon: macOS leaves it
/// behind the windows of whatever app was frontmost. Activating the app puts it in front.
fn bring_to_front() {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};
    let Some(cls) = AnyClass::get(c"NSApplication") else { return };
    unsafe {
        let app: *mut AnyObject = msg_send![cls, sharedApplication];
        if !app.is_null() {
            let _: () = msg_send![app, activateIgnoringOtherApps: true];
        }
    }
}

fn spawn_history(proxy: &EventLoopProxy<UserEvent>, range: &'static str, busy: &Arc<AtomicBool>) {
    if busy.swap(true, Ordering::AcqRel) {
        return;
    }
    let (proxy, busy) = (proxy.clone(), busy.clone());
    let _ = std::thread::Builder::new().name("packet-history".into()).spawn(move || {
        let data = store::open()
            .ok()
            .and_then(|c| history::build(&c, range, time::now_ms()).ok())
            .map(|v| v.to_string())
            .unwrap_or_else(|| "null".into());
        busy.store(false, Ordering::Release);
        let _ = proxy.send_event(UserEvent::History(range, data));
    });
}

/// No round for this long means the sampler has stopped or is stuck.
pub const STALE_MS: i64 = 3 * 60_000;
/// Test Now gives up waiting after this long and re-enables the button.
const TEST_NOW_GIVE_UP: Duration = Duration::from_secs(90);

/// True when the newest measurement (or the launch, before the first one) is older than STALE_MS.
pub fn is_stale(last_round_ms: Option<i64>, launched_ms: i64, now_ms: i64) -> bool {
    now_ms - last_round_ms.unwrap_or(launched_ms) > STALE_MS
}

/// One Packet per Mac: a second copy would double the measuring traffic. The lock is held by an
/// open descriptor for the life of the process.
fn single_instance() -> bool {
    let dir = store::data_dir();
    let _ = std::fs::create_dir_all(&dir);
    let Ok(f) = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join("packet.lock")) else {
        return true;
    };
    use std::os::fd::AsRawFd;
    let ok = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
    if ok {
        std::mem::forget(f);
    }
    ok
}

fn start_sampler(proxy: &EventLoopProxy<UserEvent>) -> (std::sync::mpsc::Sender<Cmd>, std::thread::JoinHandle<()>) {
    let p = proxy.clone();
    sampler::spawn(move |s| {
        let _ = p.send_event(UserEvent::Snap(Box::new(s)));
    })
}

struct HistoryWindow {
    window: Window,
    view: WebView,
}

pub fn run() {
    let mut event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    event_loop.set_activation_policy(ActivationPolicy::Accessory);
    let proxy = event_loop.create_proxy();
    MenuEvent::set_event_handler(Some({
        let proxy = proxy.clone();
        move |e| {
            let _ = proxy.send_event(UserEvent::Menu(e));
        }
    }));

    if !single_instance() {
        return;
    }
    let (mut cmd, sampler) = start_sampler(&proxy);
    let mut sampler = Some(sampler);
    let launched_ms = time::now_ms();
    let mut test_pending: Option<Instant> = None;

    let (menu, items) = build_menu();
    let mut menu = Some(menu);
    let mut tray: Option<TrayIcon> = None;
    let mut last: Option<Snapshot> = None;
    let mut hist: Option<HistoryWindow> = None;
    let mut range: &'static str = "24h";
    let hist_busy = Arc::new(AtomicBool::new(false));
    let mut next_tick = Instant::now() + TICK;

    event_loop.run(move |event, target, control_flow| {
        if Instant::now() >= next_tick {
            next_tick = Instant::now() + TICK;
            if let Some(s) = &last {
                show_ages(&items, s);
            }
            if test_pending.is_some_and(|t| t.elapsed() > TEST_NOW_GIVE_UP) {
                test_pending = None;
                show_test(&items, false);
                if let (Some(h), Some(s)) = (&hist, &last) {
                    let _ = h.view.evaluate_script(&format!("now({})", now_json(s, false)));
                }
            }
            let now = time::now_ms();
            if is_stale(last.as_ref().map(|s| s.round.at_ms), launched_ms, now) {
                // Never leave a green icon over old data: go grey, say so, and restart the sampler.
                if let Some(t) = &tray {
                    let _ = t.set_icon(icon_for(None));
                    let _ = t.set_tooltip(Some("Packet · no recent measurement"));
                }
                let since = last.as_ref().map_or("launch".to_string(), |s| time::hhmm(s.round.at_ms));
                items.verdict.set_text(format!("⚫ No measurement since {since}. Restarting the measurements."));
                if let (Some(h), Some(s)) = (&hist, &last) {
                    let _ = h.view.evaluate_script(&format!("now({})", stale_json(s, &since)));
                }
                if sampler.as_ref().is_none_or(|h| h.is_finished()) {
                    let (c, h) = start_sampler(&proxy);
                    cmd = c;
                    sampler = Some(h);
                }
            }
        }
        *control_flow = ControlFlow::WaitUntil(next_tick);
        match event {
            Event::NewEvents(StartCause::Init) => {
                let mut b = TrayIconBuilder::new()
                    .with_menu(Box::new(menu.take().expect("menu built once")))
                    .with_tooltip("Packet");
                if let Some(i) = icon_for(None) {
                    b = b.with_icon(i);
                }
                tray = Some(b.build().expect("create menu-bar item"));
                first_launch_login();
                show_login(&items);
                if !launched_at_login() {
                    hist = Some(open_window(target, &proxy, &mut range));
                }
            }
            Event::Reopen { .. } => match &hist {
                Some(h) => {
                    h.window.set_focus();
                    bring_to_front();
                }
                None => hist = Some(open_window(target, &proxy, &mut range)),
            },
            Event::UserEvent(UserEvent::Snap(s)) => {
                if s.manual_done {
                    test_pending = None;
                }
                if let Some(t) = &tray {
                    show(t, &items, &s);
                }
                show_test(&items, s.testing || test_pending.is_some());
                if let Some(h) = &hist {
                    let _ = h.view.evaluate_script(&format!("now({})", now_json(&s, s.testing || test_pending.is_some())));
                    spawn_history(&proxy, range, &hist_busy);
                }
                last = Some(*s);
            }
            Event::UserEvent(UserEvent::Menu(e)) => {
                if e.id == *items.quit.id() {
                    let _ = cmd.send(Cmd::Shutdown);
                    // The sampler may be mid-round; give it a moment, then go regardless.
                    if let Some(h) = sampler.take() {
                        let t = Instant::now();
                        while !h.is_finished() && t.elapsed() < Duration::from_secs(3) {
                            std::thread::sleep(Duration::from_millis(20));
                        }
                    }
                    *control_flow = ControlFlow::Exit;
                } else if e.id == *items.test.id() {
                    if test_pending.is_none() {
                        test_pending = Some(Instant::now());
                        show_test(&items, true);
                        let _ = cmd.send(Cmd::TestNow);
                    }
                } else if e.id == *items.hourly.id() {
                    let _ = sched::set_hourly(items.hourly.is_checked());
                    items.hourly.set_checked(sched::hourly_enabled());
                } else if e.id == *items.login.id() {
                    if login::state() == LoginState::RequiresApproval {
                        let _ = std::process::Command::new("/usr/bin/open").arg(LOGIN_SETTINGS).status();
                    } else {
                        let _ = login::set(items.login.is_checked());
                    }
                    show_login(&items);
                } else if e.id == *items.history.id() {
                    match &hist {
                        Some(h) => {
                            h.window.set_focus();
                            bring_to_front();
                        }
                        None => hist = Some(open_window(target, &proxy, &mut range)),
                    }
                }
            }
            Event::UserEvent(UserEvent::Ipc(body)) => match parse_page_request(&body) {
                Some(PageRequest::Range(r)) => {
                    range = r;
                    spawn_history(&proxy, range, &hist_busy);
                    if let (Some(h), Some(s)) = (&hist, &last) {
                        let _ = h.view.evaluate_script(&format!("now({})", now_json(s, test_pending.is_some())));
                    }
                }
                Some(PageRequest::Drag) => {
                    if let Some(h) = &hist {
                        let _ = h.window.drag_window();
                    }
                }
                Some(PageRequest::TestNow) => {
                    if test_pending.is_none() {
                        test_pending = Some(Instant::now());
                        show_test(&items, true);
                        let _ = cmd.send(Cmd::TestNow);
                    }
                    if let (Some(h), Some(s)) = (&hist, &last) {
                        let _ = h.view.evaluate_script(&format!("now({})", now_json(s, true)));
                    }
                }
                None => {}
            },
            Event::UserEvent(UserEvent::History(built, data)) => {
                if built != range {
                    // The range changed while this one was building; build the one now wanted.
                    spawn_history(&proxy, range, &hist_busy);
                } else if let Some(h) = &hist {
                    let _ = h.view.evaluate_script(&format!("render({data})"));
                }
            }
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                hist = None;
                target.set_activation_policy_at_runtime(ActivationPolicy::Accessory);
            }
            _ => {}
        }
    });
}

fn sha256_b64(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(Sha256::digest(data))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verdict::{findings, judge, tests::healthy, Baseline};

    #[test]
    fn sha256_known() {
        // sha256("abc")
        assert_eq!(sha256_b64(b"abc"), "ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0=");
        assert_eq!(sha256_b64(b""), "47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=");
    }

    #[test]
    fn page_locked() {
        let page = page_html();
        assert!(!page.contains("__SCRIPT_HASH__"));
        assert!(page.contains("default-src 'none'"));
        assert!(page.contains("script-src 'sha256-"));
        assert!(!page.contains("http://") && !page.contains("https://"), "the page loads nothing from the network");
        assert!(allow_navigation(HISTORY_URL));
        assert!(!allow_navigation("https://example.com/"));
    }

    #[test]
    fn stale_after_three_missed_minutes() {
        let m = 60_000;
        assert!(!is_stale(Some(10 * m), 0, 12 * m));
        assert!(is_stale(Some(10 * m), 0, 13 * m + 1), "three minutes without a round");
        assert!(!is_stale(None, 10 * m, 11 * m), "just launched");
        assert!(is_stale(None, 10 * m, 14 * m), "launched and never measured");
    }

    #[test]
    fn page_ipc() {
        assert_eq!(parse_page_request(r#"{"type":"range","range":"7d"}"#), Some(PageRequest::Range("7d")));
        assert_eq!(parse_page_request(r#"{"type":"range","range":"9y"}"#), None);
        assert_eq!(parse_page_request(r#"{"type":"test"}"#), Some(PageRequest::TestNow));
        assert_eq!(parse_page_request(r#"{"type":"drag"}"#), Some(PageRequest::Drag));
        assert_eq!(parse_page_request(r#"{"type":"other"}"#), None);
        assert_eq!(parse_page_request(&"x".repeat(MAX_IPC_BYTES + 1)), None);
    }

    #[test]
    fn etime_forms() {
        assert_eq!(parse_etime("  02:15\n"), Some(135));
        assert_eq!(parse_etime("1:02:03"), Some(3723));
        assert_eq!(parse_etime("08-20:21:23"), Some(8 * 86_400 + 20 * 3600 + 21 * 60 + 23));
        assert_eq!(parse_etime("soon"), None);
    }

    #[test]
    fn now_panel_matches_menu_rows() {
        let s = snap(healthy());
        let v: serde_json::Value = serde_json::from_str(&now_json(&s, false)).unwrap();
        assert_eq!(v["level"], "good");
        assert_eq!(v["rows"].as_array().unwrap().len(), 4);
        assert_eq!(v["rows"][1]["text"], report::edge_row(&s.round));
        assert_eq!(v["regions"].as_array().unwrap().len(), REGIONS.len());
    }

    fn snap(level_round: crate::round::Round) -> Snapshot {
        let b = Baseline::default();
        Snapshot {
            verdict: judge(&level_round, &b, None),
            findings: findings(&level_round, &b, None),
            round: level_round,
            full: None,
            quick: None,
            quick_share: None,
            testing: false,
            manual_done: false,
        }
    }

    #[test]
    fn icon_matches_verdict() {
        for l in [Level::Good, Level::Fair, Level::Poor, Level::Offline] {
            let (rgba, w, h) = decode_png(glyph_bytes(Some(l))).expect("glyph decodes");
            assert_eq!(rgba.len(), (w * h * 4) as usize);
        }
        // Each level has its own glyph colour.
        let mut seen: Vec<&[u8]> = [Level::Good, Level::Fair, Level::Poor, Level::Offline].iter().map(|l| glyph_bytes(Some(*l))).collect();
        seen.dedup();
        assert_eq!(seen.len(), 4);
        let good = snap(healthy());
        assert_eq!(good.verdict.level, Level::Good);
        assert_eq!(glyph_bytes(Some(good.verdict.level)), GLYPH_GOOD);
        assert_eq!(title_for(&good), "12 ms");
        let off = snap(crate::round::Round::empty(1));
        assert_eq!(glyph_bytes(Some(off.verdict.level)), GLYPH_OFFLINE);
        assert_eq!(title_for(&off), "Offline");
    }
}
