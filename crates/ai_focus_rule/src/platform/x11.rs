//! Responsibility: Implement Desktop on Linux with X11 (x11rb): the focused window, every
//! open window, a screenshot of the screen, minimizing a window and Ctrl+W through XTest.
//!
//! Works on an X11 session and on XWayland windows; a pure Wayland session does not let one
//! program see another's windows, so there the monitor stays idle. Window handles are X11
//! window ids. Processes are named from `_NET_WM_PID` and `/proc/<pid>/exe`, falling back to
//! the window's `WM_CLASS`.

use super::{Desktop, Foreground, OpenWindow, Screenshot};
use parking_lot::Mutex;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ClientMessageEvent, ConnectionExt as _, EventMask, ImageFormat, KeyButMask,
    MapState, Window,
};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;

const ICONIC_STATE: u32 = 3;
const KEY_PRESS: u8 = 2;
const KEY_RELEASE: u8 = 3;
const XK_CONTROL_L: u32 = 0xffe3;
const XK_W: u32 = 0x0077;

/// Responsibility: Hold the X connection (opened on first use, reopened after an error).
pub struct X11Desktop {
    conn: Mutex<Option<Conn>>,
}

/// Responsibility: Keep one X connection with its root window and the atoms it needs.
struct Conn {
    x: RustConnection,
    root: Window,
    atoms: Atoms,
}

struct Atoms {
    net_active_window: Atom,
    net_client_list: Atom,
    net_wm_name: Atom,
    net_wm_pid: Atom,
    net_wm_state: Atom,
    net_wm_state_hidden: Atom,
    utf8_string: Atom,
    wm_state: Atom,
    wm_change_state: Atom,
}

type XResult<T> = Result<T, Box<dyn std::error::Error>>;

impl X11Desktop {
    pub fn new() -> Self {
        Self {
            conn: Mutex::new(None),
        }
    }

    /// Run `f` on the connection; on any X error drop the connection so the next call
    /// reconnects (the X server may have restarted).
    fn with<T>(&self, f: impl FnOnce(&Conn) -> XResult<T>) -> Option<T> {
        let mut slot = self.conn.lock();
        if slot.is_none() {
            *slot = Conn::open().ok();
        }
        let result = f(slot.as_ref()?);
        match result {
            Ok(value) => Some(value),
            Err(err) => {
                tracing::debug!("x11: {err}");
                *slot = None;
                None
            }
        }
    }
}

impl Conn {
    fn open() -> XResult<Self> {
        let (x, screen) = x11rb::connect(None)?;
        let root = x.setup().roots[screen].root;
        let atom = |name: &str| -> XResult<Atom> {
            Ok(x.intern_atom(false, name.as_bytes())?.reply()?.atom)
        };
        let atoms = Atoms {
            net_active_window: atom("_NET_ACTIVE_WINDOW")?,
            net_client_list: atom("_NET_CLIENT_LIST")?,
            net_wm_name: atom("_NET_WM_NAME")?,
            net_wm_pid: atom("_NET_WM_PID")?,
            net_wm_state: atom("_NET_WM_STATE")?,
            net_wm_state_hidden: atom("_NET_WM_STATE_HIDDEN")?,
            utf8_string: atom("UTF8_STRING")?,
            wm_state: atom("WM_STATE")?,
            wm_change_state: atom("WM_CHANGE_STATE")?,
        };
        Ok(Self { x, root, atoms })
    }

    fn u32s(&self, window: Window, property: Atom) -> XResult<Vec<u32>> {
        let reply = self
            .x
            .get_property(false, window, property, AtomEnum::ANY, 0, 1024)?
            .reply()?;
        Ok(reply.value32().map(|v| v.collect()).unwrap_or_default())
    }

    fn title(&self, window: Window) -> XResult<String> {
        let utf8 = self
            .x
            .get_property(
                false,
                window,
                self.atoms.net_wm_name,
                self.atoms.utf8_string,
                0,
                1024,
            )?
            .reply()?;
        if !utf8.value.is_empty() {
            return Ok(String::from_utf8_lossy(&utf8.value).into_owned());
        }
        let plain = self
            .x
            .get_property(false, window, AtomEnum::WM_NAME, AtomEnum::ANY, 0, 1024)?
            .reply()?;
        Ok(String::from_utf8_lossy(&plain.value).into_owned())
    }

    /// `/proc/<pid>/exe` file name, else `comm`, else the `WM_CLASS` class.
    fn process(&self, window: Window) -> XResult<(u32, String)> {
        let pid = self
            .u32s(window, self.atoms.net_wm_pid)?
            .first()
            .copied()
            .unwrap_or(0);
        if pid != 0 {
            let exe = std::fs::read_link(format!("/proc/{pid}/exe"))
                .ok()
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
            let comm = std::fs::read_to_string(format!("/proc/{pid}/comm"))
                .ok()
                .map(|c| c.trim().to_string());
            if let Some(name) = exe.or(comm).filter(|n| !n.is_empty()) {
                return Ok((pid, name));
            }
        }
        let class = self
            .x
            .get_property(false, window, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 256)?
            .reply()?;
        // WM_CLASS is "instance\0class\0": the class names the program.
        let name = class
            .value
            .split(|b| *b == 0)
            .rfind(|part| !part.is_empty())
            .map(|c| String::from_utf8_lossy(c).into_owned())
            .unwrap_or_default();
        Ok((pid, name))
    }

    /// The window itself if it has a title, else its first titled child (a window
    /// manager's frame holds the client window as a child).
    fn titled(&self, window: Window) -> XResult<Option<Window>> {
        if !self.title(window)?.trim().is_empty() {
            return Ok(Some(window));
        }
        for child in self.x.query_tree(window)?.reply()?.children {
            if !self.title(child)?.trim().is_empty() {
                return Ok(Some(child));
            }
        }
        Ok(None)
    }

    fn foreground(&self) -> XResult<Option<Window>> {
        let active = self
            .u32s(self.root, self.atoms.net_active_window)?
            .first()
            .copied()
            .unwrap_or(0);
        if active != 0 {
            return self.titled(active);
        }
        // No window manager: walk up from the focused window to a titled one.
        let mut window = self.x.get_input_focus()?.reply()?.focus;
        while window > 1 && window != self.root {
            if !self.title(window)?.trim().is_empty() {
                return Ok(Some(window));
            }
            window = self.x.query_tree(window)?.reply()?.parent;
        }
        Ok(None)
    }

    fn minimized(&self, window: Window) -> XResult<bool> {
        if self.u32s(window, self.atoms.wm_state)?.first() == Some(&ICONIC_STATE) {
            return Ok(true);
        }
        Ok(self
            .u32s(window, self.atoms.net_wm_state)?
            .contains(&self.atoms.net_wm_state_hidden))
    }

    /// Managed windows (`_NET_CLIENT_LIST`), or without a window manager the mapped
    /// top-level windows.
    fn open_windows(&self) -> XResult<Vec<Window>> {
        let managed = self.u32s(self.root, self.atoms.net_client_list)?;
        if !managed.is_empty() {
            return Ok(managed);
        }
        let mut out = Vec::new();
        for child in self.x.query_tree(self.root)?.reply()?.children {
            let attributes = self.x.get_window_attributes(child)?.reply()?;
            if attributes.map_state == MapState::VIEWABLE && !attributes.override_redirect {
                if let Some(window) = self.titled(child)? {
                    out.push(window);
                }
            }
        }
        Ok(out)
    }

    fn describe(&self, window: Window) -> XResult<Foreground> {
        let (pid, process_name) = self.process(window)?;
        Ok(Foreground {
            window: window as isize,
            pid,
            process_name,
            title: self.title(window)?,
        })
    }

    fn keycode(&self, keysym: u32) -> XResult<Option<u8>> {
        let setup = self.x.setup();
        let (min, max) = (setup.min_keycode, setup.max_keycode);
        let map = self.x.get_keyboard_mapping(min, max - min + 1)?.reply()?;
        let per = map.keysyms_per_keycode.max(1) as usize;
        Ok(map
            .keysyms
            .chunks(per)
            .position(|syms| syms.contains(&keysym))
            .map(|i| min + i as u8))
    }
}

impl Desktop for X11Desktop {
    fn foreground(&self) -> Option<Foreground> {
        self.with(|c| match c.foreground()? {
            Some(window) => c.describe(window).map(Some),
            None => Ok(None),
        })
        .flatten()
    }

    fn screenshot(&self, _fg: &Foreground) -> Option<Screenshot> {
        self.with(|c| {
            let geometry = c.x.get_geometry(c.root)?.reply()?;
            let image =
                c.x.get_image(
                    ImageFormat::Z_PIXMAP,
                    c.root,
                    0,
                    0,
                    geometry.width,
                    geometry.height,
                    !0,
                )?
                .reply()?;
            let pixels = geometry.width as usize * geometry.height as usize;
            if image.data.len() < pixels * 4 {
                return Err("unsupported screen depth".into());
            }
            // 24/32-bit TrueColor: BGRX in memory.
            let mut rgba = Vec::with_capacity(pixels * 4);
            for px in image.data.chunks_exact(4).take(pixels) {
                rgba.extend_from_slice(&[px[2], px[1], px[0], 255]);
            }
            Ok(Screenshot {
                width: geometry.width as u32,
                height: geometry.height as u32,
                rgba,
            })
        })
    }

    fn visible_processes(&self) -> Vec<String> {
        self.open_windows()
            .into_iter()
            .filter(|w| !w.minimized)
            .map(|w| w.process_name)
            .collect()
    }

    fn close_active_tab(&self, fg: &Foreground) -> bool {
        self.with(|c| {
            // Same window and same tab, checked right before the keys go out.
            let now = c.foreground()?;
            if now != Some(fg.window as Window) || c.title(fg.window as Window)? != fg.title {
                return Ok(false);
            }
            // Keys the user holds would combine with ours (Shift+Ctrl+W closes the
            // window): try again on a later sample.
            let pointer = c.x.query_pointer(c.root)?.reply()?;
            let held =
                KeyButMask::SHIFT | KeyButMask::CONTROL | KeyButMask::MOD1 | KeyButMask::MOD4;
            if u16::from(pointer.mask) & u16::from(held) != 0 {
                return Ok(false);
            }
            let (Some(ctrl), Some(w)) = (c.keycode(XK_CONTROL_L)?, c.keycode(XK_W)?) else {
                return Ok(false);
            };
            for (kind, key) in [
                (KEY_PRESS, ctrl),
                (KEY_PRESS, w),
                (KEY_RELEASE, w),
                (KEY_RELEASE, ctrl),
            ] {
                c.x.xtest_fake_input(kind, key, 0, c.root, 0, 0, 0)?;
            }
            c.x.flush()?;
            Ok(true)
        })
        .unwrap_or(false)
    }

    fn open_windows(&self) -> Vec<OpenWindow> {
        self.with(|c| {
            let mut out = Vec::new();
            for window in c.open_windows()? {
                let fg = c.describe(window)?;
                if fg.title.trim().is_empty() {
                    continue;
                }
                out.push(OpenWindow {
                    window: fg.window,
                    pid: fg.pid,
                    process_name: fg.process_name,
                    title: fg.title,
                    minimized: c.minimized(window)?,
                });
            }
            Ok(out)
        })
        .unwrap_or_default()
    }

    fn minimize(&self, window: isize) -> bool {
        self.with(|c| {
            // ICCCM: ask the window manager to iconify the window.
            let event = ClientMessageEvent::new(
                32,
                window as Window,
                c.atoms.wm_change_state,
                [ICONIC_STATE, 0, 0, 0, 0],
            );
            c.x.send_event(
                false,
                c.root,
                EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
                event,
            )?;
            c.x.flush()?;
            Ok(true)
        })
        .unwrap_or(false)
    }
}

/// Needs an X server: run with `xvfb-run cargo test`; skipped without `DISPLAY`.
#[cfg(test)]
mod tests {
    use super::*;
    use x11rb::protocol::xproto::{
        CreateWindowAux, InputFocus, PropMode, WindowClass, CLIENT_MESSAGE_EVENT,
    };
    use x11rb::protocol::Event;
    use x11rb::wrapper::ConnectionExt as _;

    #[test]
    fn x11_desktop_sees_and_controls_a_window() {
        if std::env::var_os("DISPLAY").is_none() {
            eprintln!("skipped: no DISPLAY (run under xvfb-run)");
            return;
        }
        // A test window, owned by this process, playing "a browser tab".
        let (x, screen) = x11rb::connect(None).unwrap();
        let root = x.setup().roots[screen].root;
        let win = x.generate_id().unwrap();
        x.create_window(
            0,
            win,
            root,
            0,
            0,
            320,
            200,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new().event_mask(EventMask::KEY_PRESS),
        )
        .unwrap();
        x.change_property8(
            PropMode::REPLACE,
            win,
            AtomEnum::WM_NAME,
            AtomEnum::STRING,
            b"Funny cats - YouTube",
        )
        .unwrap();
        let pid_atom = x
            .intern_atom(false, b"_NET_WM_PID")
            .unwrap()
            .reply()
            .unwrap()
            .atom;
        x.change_property32(
            PropMode::REPLACE,
            win,
            pid_atom,
            AtomEnum::CARDINAL,
            &[std::process::id()],
        )
        .unwrap();
        x.map_window(win).unwrap();
        x.sync().unwrap();
        x.set_input_focus(InputFocus::PARENT, win, 0u32).unwrap();
        // Act as the window manager, to see the minimize request.
        x.change_window_attributes(
            root,
            &x11rb::protocol::xproto::ChangeWindowAttributesAux::new()
                .event_mask(EventMask::SUBSTRUCTURE_REDIRECT),
        )
        .unwrap();
        x.sync().unwrap();

        let desktop = X11Desktop::new();
        let fg = desktop.foreground().expect("a focused window");
        assert_eq!(fg.window, win as isize);
        assert_eq!(fg.title, "Funny cats - YouTube");
        assert_eq!(fg.pid, std::process::id());
        let exe = std::env::current_exe().unwrap();
        assert_eq!(fg.process_name, exe.file_name().unwrap().to_string_lossy());

        let open = desktop.open_windows();
        assert!(
            open.iter()
                .any(|w| w.window == win as isize && !w.minimized),
            "{open:?}"
        );
        assert!(desktop.visible_processes().contains(&fg.process_name));
        let shot = desktop.screenshot(&fg).expect("a screenshot");
        assert!(shot.width > 0 && shot.rgba.len() == (shot.width * shot.height * 4) as usize);

        // Ctrl+W reaches the window; a minimize request reaches the "window manager".
        assert!(desktop.close_active_tab(&fg));
        assert!(desktop.minimize(win as isize));
        let (mut key_with_ctrl, mut iconify) = (false, false);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline && !(key_with_ctrl && iconify) {
            match x.poll_for_event().unwrap() {
                Some(Event::KeyPress(e)) => {
                    key_with_ctrl |= u16::from(e.state) & u16::from(KeyButMask::CONTROL) != 0
                }
                Some(Event::ClientMessage(e)) if e.response_type & 0x7f == CLIENT_MESSAGE_EVENT => {
                    iconify |= e.window == win && e.data.as_data32()[0] == ICONIC_STATE
                }
                Some(_) => {}
                None => std::thread::sleep(std::time::Duration::from_millis(20)),
            }
        }
        assert!(key_with_ctrl, "Ctrl+W did not arrive");
        assert!(iconify, "the minimize request did not arrive");

        // Another title in the same window: Ctrl+W is not sent (the user switched tabs).
        x.change_property8(
            PropMode::REPLACE,
            win,
            AtomEnum::WM_NAME,
            AtomEnum::STRING,
            b"Rust docs",
        )
        .unwrap();
        x.sync().unwrap();
        assert!(!desktop.close_active_tab(&fg));
    }
}
