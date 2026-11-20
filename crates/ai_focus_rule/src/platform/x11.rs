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
    display: Mutex<Option<X11Display>>,
}

/// Responsibility: Keep one X connection with its root window and the atoms it needs.
struct X11Display {
    connection: RustConnection,
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
            display: Mutex::new(None),
        }
    }

    /// Run `operation` on the connection; on any X error drop the connection so the next call
    /// reconnects (the X server may have restarted).
    fn with<T>(&self, operation: impl FnOnce(&X11Display) -> XResult<T>) -> Option<T> {
        let mut slot = self.display.lock();
        if slot.is_none() {
            *slot = X11Display::open().ok();
        }
        let result = operation(slot.as_ref()?);
        match result {
            Ok(value) => Some(value),
            Err(error) => {
                tracing::debug!("x11: {error}");
                *slot = None;
                None
            }
        }
    }
}

impl X11Display {
    fn open() -> XResult<Self> {
        let (connection, screen_index) = x11rb::connect(None)?;
        let root = connection.setup().roots[screen_index].root;
        let atom = |name: &str| -> XResult<Atom> {
            Ok(connection
                .intern_atom(false, name.as_bytes())?
                .reply()?
                .atom)
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
        Ok(Self {
            connection,
            root,
            atoms,
        })
    }

    fn u32_property_values(&self, window: Window, property: Atom) -> XResult<Vec<u32>> {
        let reply = self
            .connection
            .get_property(false, window, property, AtomEnum::ANY, 0, 1024)?
            .reply()?;
        Ok(reply
            .value32()
            .map(|values| values.collect())
            .unwrap_or_default())
    }

    fn title(&self, window: Window) -> XResult<String> {
        let utf8 = self
            .connection
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
            .connection
            .get_property(false, window, AtomEnum::WM_NAME, AtomEnum::ANY, 0, 1024)?
            .reply()?;
        Ok(String::from_utf8_lossy(&plain.value).into_owned())
    }

    /// `/proc/<pid>/exe` file name, else `comm`, else the `WM_CLASS` class.
    fn process(&self, window: Window) -> XResult<(u32, String)> {
        let process_id = self
            .u32_property_values(window, self.atoms.net_wm_pid)?
            .first()
            .copied()
            .unwrap_or(0);
        if process_id != 0 {
            let executable_name = std::fs::read_link(format!("/proc/{process_id}/exe"))
                .ok()
                .and_then(|executable_path| {
                    executable_path
                        .file_name()
                        .map(|file_name| file_name.to_string_lossy().into_owned())
                });
            let command_name = std::fs::read_to_string(format!("/proc/{process_id}/comm"))
                .ok()
                .map(|contents| contents.trim().to_string());
            if let Some(name) = executable_name
                .or(command_name)
                .filter(|name| !name.is_empty())
            {
                return Ok((process_id, name));
            }
        }
        let class = self
            .connection
            .get_property(false, window, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 256)?
            .reply()?;
        // WM_CLASS is "instance\0class\0": the class names the program.
        let name = class
            .value
            .split(|byte| *byte == 0)
            .rfind(|part| !part.is_empty())
            .map(|class_name| String::from_utf8_lossy(class_name).into_owned())
            .unwrap_or_default();
        Ok((process_id, name))
    }

    /// The window itself if it has a title, else its first titled child (a window
    /// manager's frame holds the client window as a child).
    fn titled(&self, window: Window) -> XResult<Option<Window>> {
        if !self.title(window)?.trim().is_empty() {
            return Ok(Some(window));
        }
        for child in self.connection.query_tree(window)?.reply()?.children {
            if !self.title(child)?.trim().is_empty() {
                return Ok(Some(child));
            }
        }
        Ok(None)
    }

    fn foreground(&self) -> XResult<Option<Window>> {
        let active = self
            .u32_property_values(self.root, self.atoms.net_active_window)?
            .first()
            .copied()
            .unwrap_or(0);
        if active != 0 {
            return self.titled(active);
        }
        // No window manager: walk up from the focused window to a titled one.
        let mut window = self.connection.get_input_focus()?.reply()?.focus;
        while window > 1 && window != self.root {
            if !self.title(window)?.trim().is_empty() {
                return Ok(Some(window));
            }
            window = self.connection.query_tree(window)?.reply()?.parent;
        }
        Ok(None)
    }

    fn minimized(&self, window: Window) -> XResult<bool> {
        if self
            .u32_property_values(window, self.atoms.wm_state)?
            .first()
            == Some(&ICONIC_STATE)
        {
            return Ok(true);
        }
        Ok(self
            .u32_property_values(window, self.atoms.net_wm_state)?
            .contains(&self.atoms.net_wm_state_hidden))
    }

    /// Managed windows (`_NET_CLIENT_LIST`), or without a window manager the mapped
    /// top-level windows.
    fn open_windows(&self) -> XResult<Vec<Window>> {
        let managed = self.u32_property_values(self.root, self.atoms.net_client_list)?;
        if !managed.is_empty() {
            return Ok(managed);
        }
        let mut windows = Vec::new();
        for child in self.connection.query_tree(self.root)?.reply()?.children {
            let attributes = self.connection.get_window_attributes(child)?.reply()?;
            if attributes.map_state == MapState::VIEWABLE && !attributes.override_redirect {
                if let Some(window) = self.titled(child)? {
                    windows.push(window);
                }
            }
        }
        Ok(windows)
    }

    fn describe(&self, window: Window) -> XResult<Foreground> {
        let (process_id, process_name) = self.process(window)?;
        Ok(Foreground {
            window: window as isize,
            pid: process_id,
            process_name,
            title: self.title(window)?,
        })
    }

    fn keycode(&self, keysym: u32) -> XResult<Option<u8>> {
        let setup = self.connection.setup();
        let (min_keycode, max_keycode) = (setup.min_keycode, setup.max_keycode);
        let keyboard_mapping = self
            .connection
            .get_keyboard_mapping(min_keycode, max_keycode - min_keycode + 1)?
            .reply()?;
        let keysyms_per_keycode = keyboard_mapping.keysyms_per_keycode.max(1) as usize;
        Ok(keyboard_mapping
            .keysyms
            .chunks(keysyms_per_keycode)
            .position(|keycode_keysyms| keycode_keysyms.contains(&keysym))
            .map(|keycode_offset| min_keycode + keycode_offset as u8))
    }
}

impl Desktop for X11Desktop {
    fn foreground(&self) -> Option<Foreground> {
        self.with(|display| match display.foreground()? {
            Some(window) => display.describe(window).map(Some),
            None => Ok(None),
        })
        .flatten()
    }

    fn screenshot(&self, _foreground: &Foreground) -> Option<Screenshot> {
        self.with(|display| {
            let geometry = display.connection.get_geometry(display.root)?.reply()?;
            let image = display
                .connection
                .get_image(
                    ImageFormat::Z_PIXMAP,
                    display.root,
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
            for pixel in image.data.chunks_exact(4).take(pixels) {
                rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], 255]);
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
            .filter(|window| !window.minimized)
            .map(|window| window.process_name)
            .collect()
    }

    fn close_active_tab(&self, foreground: &Foreground) -> bool {
        self.with(|display| {
            // Same window and same tab, checked right before the keys go out.
            let current_foreground = display.foreground()?;
            if current_foreground != Some(foreground.window as Window)
                || display.title(foreground.window as Window)? != foreground.title
            {
                return Ok(false);
            }
            // Keys the user holds would combine with ours (Shift+Ctrl+W closes the
            // window): try again on a later sample.
            let pointer = display.connection.query_pointer(display.root)?.reply()?;
            let held =
                KeyButMask::SHIFT | KeyButMask::CONTROL | KeyButMask::MOD1 | KeyButMask::MOD4;
            if u16::from(pointer.mask) & u16::from(held) != 0 {
                return Ok(false);
            }
            let (Some(control_keycode), Some(w_keycode)) =
                (display.keycode(XK_CONTROL_L)?, display.keycode(XK_W)?)
            else {
                return Ok(false);
            };
            for (kind, key) in [
                (KEY_PRESS, control_keycode),
                (KEY_PRESS, w_keycode),
                (KEY_RELEASE, w_keycode),
                (KEY_RELEASE, control_keycode),
            ] {
                display
                    .connection
                    .xtest_fake_input(kind, key, 0, display.root, 0, 0, 0)?;
            }
            display.connection.flush()?;
            Ok(true)
        })
        .unwrap_or(false)
    }

    fn open_windows(&self) -> Vec<OpenWindow> {
        self.with(|display| {
            let mut windows = Vec::new();
            for window in display.open_windows()? {
                let described = display.describe(window)?;
                if described.title.trim().is_empty() {
                    continue;
                }
                windows.push(OpenWindow {
                    window: described.window,
                    pid: described.pid,
                    process_name: described.process_name,
                    title: described.title,
                    minimized: display.minimized(window)?,
                });
            }
            Ok(windows)
        })
        .unwrap_or_default()
    }

    fn minimize(&self, window: isize) -> bool {
        self.with(|display| {
            // ICCCM: ask the window manager to iconify the window.
            let event = ClientMessageEvent::new(
                32,
                window as Window,
                display.atoms.wm_change_state,
                [ICONIC_STATE, 0, 0, 0, 0],
            );
            display.connection.send_event(
                false,
                display.root,
                EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
                event,
            )?;
            display.connection.flush()?;
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
        let (connection, screen_index) = x11rb::connect(None).unwrap();
        let root = connection.setup().roots[screen_index].root;
        let window = connection.generate_id().unwrap();
        connection
            .create_window(
                0,
                window,
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
        connection
            .change_property8(
                PropMode::REPLACE,
                window,
                AtomEnum::WM_NAME,
                AtomEnum::STRING,
                b"Funny cats - YouTube",
            )
            .unwrap();
        let pid_atom = connection
            .intern_atom(false, b"_NET_WM_PID")
            .unwrap()
            .reply()
            .unwrap()
            .atom;
        connection
            .change_property32(
                PropMode::REPLACE,
                window,
                pid_atom,
                AtomEnum::CARDINAL,
                &[std::process::id()],
            )
            .unwrap();
        connection.map_window(window).unwrap();
        connection.sync().unwrap();
        connection
            .set_input_focus(InputFocus::PARENT, window, 0u32)
            .unwrap();
        // Act as the window manager, to see the minimize request.
        connection
            .change_window_attributes(
                root,
                &x11rb::protocol::xproto::ChangeWindowAttributesAux::new()
                    .event_mask(EventMask::SUBSTRUCTURE_REDIRECT),
            )
            .unwrap();
        connection.sync().unwrap();

        let desktop = X11Desktop::new();
        let foreground = desktop.foreground().expect("a focused window");
        assert_eq!(foreground.window, window as isize);
        assert_eq!(foreground.title, "Funny cats - YouTube");
        assert_eq!(foreground.pid, std::process::id());
        let executable = std::env::current_exe().unwrap();
        assert_eq!(
            foreground.process_name,
            executable.file_name().unwrap().to_string_lossy()
        );

        let open = desktop.open_windows();
        assert!(
            open.iter()
                .any(|open_window| open_window.window == window as isize && !open_window.minimized),
            "{open:?}"
        );
        assert!(desktop
            .visible_processes()
            .contains(&foreground.process_name));
        let screenshot = desktop.screenshot(&foreground).expect("a screenshot");
        assert!(
            screenshot.width > 0
                && screenshot.rgba.len() == (screenshot.width * screenshot.height * 4) as usize
        );

        // Ctrl+W reaches the window; a minimize request reaches the "window manager".
        assert!(desktop.close_active_tab(&foreground));
        assert!(desktop.minimize(window as isize));
        let (mut key_with_ctrl, mut iconify) = (false, false);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline && !(key_with_ctrl && iconify) {
            match connection.poll_for_event().unwrap() {
                Some(Event::KeyPress(event)) => {
                    key_with_ctrl |= u16::from(event.state) & u16::from(KeyButMask::CONTROL) != 0
                }
                Some(Event::ClientMessage(event))
                    if event.response_type & 0x7f == CLIENT_MESSAGE_EVENT =>
                {
                    iconify |= event.window == window && event.data.as_data32()[0] == ICONIC_STATE
                }
                Some(_) => {}
                None => std::thread::sleep(std::time::Duration::from_millis(20)),
            }
        }
        assert!(key_with_ctrl, "Ctrl+W did not arrive");
        assert!(iconify, "the minimize request did not arrive");

        // Another title in the same window: Ctrl+W is not sent (the user switched tabs).
        connection
            .change_property8(
                PropMode::REPLACE,
                window,
                AtomEnum::WM_NAME,
                AtomEnum::STRING,
                b"Rust docs",
            )
            .unwrap();
        connection.sync().unwrap();
        assert!(!desktop.close_active_tab(&foreground));
    }
}
