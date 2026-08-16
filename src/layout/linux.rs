//! Reading and setting the keyboard layout on Linux, on whatever is running
//! the session.
//!
//! There is no such thing as "the Linux way" to do this. X11 has one (the XKB
//! group, settable by anyone who can open the display) and Wayland deliberately
//! has none — the compositor owns the keymap, and each one exposes its own
//! private way to ask. So this module holds five [`Backend`]s, probed once at
//! startup in order of specificity, and every one of them answers the same
//! three questions:
//!
//! * [`Backend::names`] — the layouts configured, in order
//! * [`Backend::current`] — which of them is live
//! * [`Backend::set_index`] — make the *n*th one live
//!
//! Everything above that is shared, which is what removes the assumption this
//! module used to be built on. It supported Hyprland only, and hardcoded
//! *layout 0 is English, layout 1 is Hebrew* — so a user with `us,il,ru` had
//! ReCast switching to Russian, and everyone on GNOME, KDE, sway or plain X11
//! had the headline feature silently do nothing at all. The index is now looked
//! up in the list the session reports.
//!
//! # Cost
//!
//! [`crate::layout::current_layout`] is consulted once per word (behind a
//! 300 ms cache) and [`switch_layout_to`] sits in the retype path, so a
//! subprocess here is not free: shelling out to `hyprctl` was measured at
//! **6.3 ms** against **0.11 ms** for the same question over Hyprland's socket,
//! and a correction that changes layout asks twice. Hyprland, sway and X11
//! therefore talk to the session directly — two socket protocols and a bit of
//! Xlib. GNOME and KDE do shell out, because their answer lives behind D-Bus
//! and a D-Bus client is a great deal more code than these two calls are worth;
//! the cache is what makes that survivable.

use std::sync::OnceLock;
use std::time::Instant;

use super::{language_of_keymap, set_layout_cache, LayoutSwitch};
use crate::types::Language;

// ─────────────────────────────────────────────────────────────────────────────
// The backend, chosen once
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Backend {
    /// Hyprland's control socket.
    Hyprland,
    /// sway (and i3-compatible compositors) over the i3 IPC socket.
    Sway,
    /// KDE Plasma's `org.kde.KeyboardLayouts` D-Bus interface, via a helper.
    Kde,
    /// GNOME's `org.gnome.desktop.input-sources` GSettings keys.
    Gnome,
    /// The XKB group, for any X11 session — including GNOME and KDE on X11,
    /// and every window manager that has no opinion of its own.
    X11,
    /// Nothing here can drive the layout. Corrections that would need a switch
    /// are declined rather than typed out under the wrong keymap.
    None,
}

/// Which backend this session gets, decided once.
///
/// Order is specificity, not preference: a compositor that owns the keymap has
/// to be asked in its own language, and X11 is the catch-all underneath because
/// XKB works under any X server whatever is drawing the windows. GNOME and KDE
/// come before it even on X11, because they manage input sources *above* XKB —
/// setting the group under GNOME works for exactly as long as it takes
/// gnome-shell to set it back.
fn backend() -> Backend {
    static BACKEND: OnceLock<Backend> = OnceLock::new();
    *BACKEND.get_or_init(|| {
        // An escape hatch for a session the probe reads wrongly — a compositor
        // that sets XDG_CURRENT_DESKTOP to something unexpected, or an X11
        // session under a desktop that also answers on D-Bus. Named rather than
        // numbered so `--status` and this agree on the vocabulary.
        if let Some(forced) = crate::settings::get("RECAST_LAYOUT_BACKEND") {
            let chosen = match forced.trim().to_lowercase().as_str() {
                "hyprland" => Some(Backend::Hyprland),
                "sway" => Some(Backend::Sway),
                "kde" => Some(Backend::Kde),
                "gnome" => Some(Backend::Gnome),
                "x11" => Some(Backend::X11),
                "none" | "off" => Some(Backend::None),
                _ => None,
            };
            match chosen {
                Some(b) => return b,
                None => eprintln!(
                    "Unknown layout backend {forced:?} — expected one of \
                     hyprland, sway, kde, gnome, x11, none. Detecting instead."
                ),
            }
        }

        let desktop = std::env::var("XDG_CURRENT_DESKTOP")
            .unwrap_or_default()
            .to_uppercase();
        if hypr::available() {
            Backend::Hyprland
        } else if sway::available() {
            Backend::Sway
        } else if desktop.contains("KDE") || std::env::var_os("KDE_FULL_SESSION").is_some() {
            Backend::Kde
        } else if desktop.contains("GNOME") || desktop.contains("UNITY") {
            Backend::Gnome
        } else if x11::available() {
            Backend::X11
        } else {
            Backend::None
        }
    })
}

impl Backend {
    /// The layouts this session is configured with, in the order the session
    /// numbers them. `None` when the question could not be answered at all,
    /// which is different from an empty list.
    fn names(self) -> Option<Vec<String>> {
        match self {
            Backend::Hyprland => hypr::names(),
            Backend::Sway => sway::names(),
            Backend::Kde => kde::names(),
            Backend::Gnome => gnome::names(),
            Backend::X11 => x11::names(),
            Backend::None => None,
        }
    }

    /// The language of the layout that is live right now.
    fn current(self) -> Option<Language> {
        match self {
            // Hyprland reports the active keymap by name per device, which is
            // more direct than an index — and, unlike an index, it is right
            // even when a device has its own layout list.
            Backend::Hyprland => hypr::current(),
            _ => {
                let names = self.names()?;
                let index = self.current_index()?;
                language_of_keymap(names.get(index)?)
            }
        }
    }

    fn current_index(self) -> Option<usize> {
        match self {
            Backend::Hyprland => None,
            Backend::Sway => sway::current_index(),
            Backend::Kde => kde::current_index(),
            Backend::Gnome => gnome::current_index(),
            Backend::X11 => x11::current_index(),
            Backend::None => None,
        }
    }

    /// Make the `index`th configured layout live. `false` if the request was
    /// refused; it says nothing about whether the switch has *landed* yet,
    /// which is what [`switch_layout_to`] polls for.
    fn set_index(self, index: usize) -> bool {
        match self {
            Backend::Hyprland => hypr::set_index(index),
            Backend::Sway => sway::set_index(index),
            Backend::Kde => kde::set_index(index),
            Backend::Gnome => gnome::set_index(index),
            Backend::X11 => x11::set_index(index),
            Backend::None => false,
        }
    }

    /// A stream that yields once every time the session changes the layout, or
    /// `None` when this backend has no way to say so and polling is all there
    /// is.
    ///
    /// Nothing about *which* layout is carried: the watcher asks in the ordinary
    /// way once it is told there is something to ask about. That keeps the
    /// per-backend part down to "recognise an interesting line", instead of four
    /// more payload formats to parse, and it is what moves the query off the
    /// thread a correction is waiting on.
    fn subscribe(self) -> Option<Signals> {
        match self {
            Backend::Hyprland => hypr::subscribe(),
            Backend::Sway => sway::subscribe(),
            Backend::Kde => kde::subscribe(),
            Backend::Gnome => gnome::subscribe(),
            // XKB does have a change event (`XkbStateNotify`), but this backend
            // already answers over a connection it keeps open, so the query it
            // would save costs a round trip rather than a process — the poll it
            // would replace is the cheap one.
            Backend::X11 | Backend::None => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Backend::Hyprland => "Hyprland (control socket)",
            Backend::Sway => "sway (i3 IPC socket)",
            Backend::Kde => "KDE Plasma (D-Bus)",
            Backend::Gnome => "GNOME (GSettings input-sources)",
            Backend::X11 => "X11 (XKB group)",
            Backend::None => "none — layout switching unavailable",
        }
    }
}

/// What `--status` says about layout switching here, and why it matters: on a
/// session with no backend the layout pipeline is off and nothing else says so.
pub fn describe_backend() -> String {
    let b = backend();
    match b.names() {
        Some(names) if !names.is_empty() => {
            let known = names
                .iter()
                .filter(|n| language_of_keymap(n).is_some())
                .count();
            let mut out = format!("{} — layouts: {}", b.label(), names.join(", "));
            if known < 2 {
                out.push_str(
                    "  ! ReCast needs an English and a Hebrew layout configured; \
                     it can only switch between layouts you already have.",
                );
            }
            out
        }
        _ => b.label().to_string(),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The two shared operations
// ─────────────────────────────────────────────────────────────────────────────

pub fn query_layout() -> Option<Language> {
    backend().current()
}

pub fn switch_layout_to(lang: Language) -> LayoutSwitch {
    // Already on the requested layout. Nothing to send — and, unlike before,
    // this is reported as the success it is.
    if super::current_layout() == Some(lang) {
        return LayoutSwitch::AlreadyThere;
    }

    let backend = backend();
    // The index is looked up rather than assumed. `us,il` puts Hebrew at 1 and
    // `il,us,ru` puts it at 0; the old hardcoded 0/1 was right for exactly one
    // of those and switched a third of users into Russian.
    let Some(names) = backend.names() else {
        return LayoutSwitch::Failed;
    };
    let Some(index) = names
        .iter()
        .position(|n| language_of_keymap(n) == Some(lang))
    else {
        // The layout simply is not installed. Nothing to do about it here, and
        // nothing to say about it every time either — the complaint belongs in
        // `--status`, which reports the configured list.
        return LayoutSwitch::Failed;
    };

    if !backend.set_index(index) {
        return LayoutSwitch::Failed;
    }

    // Then wait for it to actually be live. Accepting the command is not the
    // same as having applied it — measured at 0.3–0.9 ms apart on an idle
    // Hyprland — and `uinput` speaks keycodes, so a word injected inside that
    // gap is spelled out under the *old* layout and arrives as garbage. macOS
    // and Windows have polled for this all along; Linux fired and hoped, which
    // is why corrections involving a layout change failed intermittently.
    let gaps = crate::timing::injection();
    let deadline = Instant::now() + gaps.layout_confirm;
    loop {
        if query_layout() == Some(lang) {
            set_layout_cache(lang);
            return LayoutSwitch::Switched;
        }
        if Instant::now() >= deadline {
            return LayoutSwitch::Failed;
        }
        crate::timing::pause(gaps.layout_poll);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Watching for a layout change, instead of asking whether there has been one
// ─────────────────────────────────────────────────────────────────────────────

/// A stream of "the layout just changed" signals. Blocking: the thread reading
/// one has nothing else to do.
type Signals = Box<dyn Iterator<Item = ()> + Send>;

/// Follow the session's own layout-change notifications and keep the cache
/// current, so a correction never has to ask.
///
/// This is the other half of [`crate::layout::invalidate`], and the more
/// general one: the hotkey guess catches the user pressing a key combination,
/// while this catches every change however it was made — a click in the tray, a
/// `swaymsg` from a script, another application's own switch.
///
/// It also takes the *cost* off the correction path. GNOME and KDE answer
/// through a subprocess, so a cache miss at the end of a word used to mean a
/// fork and an exec before the word could be corrected; now the fork happens
/// here, on a thread nobody is waiting on, and only when something has actually
/// changed.
///
/// Failure is quiet and safe by construction. A backend with no subscription, a
/// compositor that restarts and closes the socket, a `gsettings` that is not
/// there — all of them end this thread, and what is left is the 300 ms cache
/// that was the whole story before. Nothing here is load-bearing.
pub fn spawn_watcher() {
    let spawned = std::thread::Builder::new()
        .name("recast-layout".into())
        .spawn(watch_forever);
    if spawned.is_err() {
        eprintln!("Could not start the layout watcher — layout changes will be picked up within 300 ms instead.");
    }
}

fn watch_forever() {
    let Some(signals) = backend().subscribe() else {
        return;
    };
    for () in signals {
        // Ask in the ordinary way. The cache is not consulted on this path —
        // `query_layout` goes straight to the backend — so this is the fresh
        // answer, taken at the one moment it is known to have changed.
        if let Some(lang) = query_layout() {
            set_layout_cache(lang);
        }
    }
}

/// A signal stream built out of lines of text, which is what three of the four
/// sources are: two of them a subprocess's stdout, one a compositor socket.
struct Lines<R> {
    lines: std::io::Lines<R>,
    /// Whether a line means the layout changed. The rest are the other traffic
    /// on the same stream — a `gdbus monitor` reports every signal the
    /// destination emits, and Hyprland's event socket carries thirty-odd event
    /// kinds we have no interest in.
    interesting: fn(&str) -> bool,
    /// The process behind `lines`, when there is one, kept alive as long as it
    /// is being read.
    _child: Option<std::process::Child>,
}

impl<R: std::io::BufRead> Iterator for Lines<R> {
    type Item = ();

    fn next(&mut self) -> Option<()> {
        loop {
            // A read error ends the stream rather than spinning on it: the
            // socket is gone, or the child died, and neither gets better by
            // being read again.
            let line = self.lines.next()?.ok()?;
            if (self.interesting)(&line) {
                return Some(());
            }
        }
    }
}

impl<R: std::io::BufRead + Send + 'static> Lines<R> {
    fn stream(
        reader: R,
        interesting: fn(&str) -> bool,
        child: Option<std::process::Child>,
    ) -> Signals {
        Box::new(Lines {
            lines: reader.lines(),
            interesting,
            _child: child,
        })
    }
}

/// Start `bin args…` with its stdout piped, for the two backends whose
/// notifications come from a command rather than a socket.
///
/// stderr goes to `/dev/null`: this is a best-effort background watch, and a
/// client complaining on a session without the service would otherwise print
/// into whatever the daemon's stderr happens to be.
fn watch_command(bin: &str, args: &[&str], interesting: fn(&str) -> bool) -> Option<Signals> {
    let mut child = std::process::Command::new(bin)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    Some(Lines::stream(
        std::io::BufReader::new(stdout),
        interesting,
        Some(child),
    ))
}

/// Our own injector, which must never be mistaken for a keyboard the user
/// types on: it carries a layout list of its own, and reading the layout off it
/// means reporting the state of a device nobody has touched. A wrong reading
/// here is worse than none — `switch_layout_to` skips the switch when it
/// believes the layout is already right, and the correction then types itself
/// out under the layout that was actually live.
const INJECTOR: &str = "recast-injector";

// ─────────────────────────────────────────────────────────────────────────────
// Minimal JSON field access, shared by the Hyprland and sway backends.
//
// Both speak JSON and neither reply needs more than a flat field out of an
// object: pulling in a JSON crate to read three keys would be the largest
// dependency in the program.
// ─────────────────────────────────────────────────────────────────────────────

/// The value of a flat `"key": "value"` string field inside one JSON object.
fn json_str<'a>(block: &'a str, key: &str) -> Option<&'a str> {
    let at = block.find(&format!("\"{key}\""))?;
    let rest = &block[at + key.len() + 2..];
    let open = rest.find('"')?;
    let rest = &rest[open + 1..];
    let close = rest.find('"')?;
    Some(&rest[..close])
}

/// The value of a flat `"key": 3` numeric field inside one JSON object.
fn json_num(block: &str, key: &str) -> Option<usize> {
    let at = block.find(&format!("\"{key}\""))?;
    let rest = &block[at + key.len() + 2..];
    let digits: String = rest
        .trim_start_matches([':', ' '])
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

/// The elements of a flat `"key": ["a", "b"]` string-array field.
fn json_str_array(block: &str, key: &str) -> Option<Vec<String>> {
    let at = block.find(&format!("\"{key}\""))?;
    let rest = &block[at..];
    let open = rest.find('[')?;
    let close = rest.find(']')?;
    if close < open {
        return None;
    }
    Some(
        rest[open + 1..close]
            .split(',')
            .filter_map(|item| {
                let item = item.trim();
                item.strip_prefix('"')?.strip_suffix('"').map(str::to_string)
            })
            .collect(),
    )
}

// ─────────────────────────────────────────────────────────────────────────────
// Hyprland — the control socket
// ─────────────────────────────────────────────────────────────────────────────

/// Talk to Hyprland over its control socket instead of spawning `hyprctl`.
///
/// `hyprctl` is a small C++ program whose entire job is to write the command to
/// this socket and print the reply. Shelling out to it cost a fork, an exec, a
/// dynamic link and a wait — measured at **6.3 ms per call** against **0.11 ms**
/// for the socket, a 57× difference — and a correction that changes layout
/// makes two such calls. That was the single largest cost in the retype path,
/// larger than every injection gap in `crate::timing` put together.
///
/// It also removes a dependency on `hyprctl` being installed and on `PATH` for
/// whatever session started the daemon.
mod hypr {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    use std::path::PathBuf;
    use std::sync::OnceLock;
    use std::time::Duration;

    use super::{json_str, language_of_keymap, Language, INJECTOR};

    /// A wedged compositor must not wedge the injection thread with it: this
    /// runs on the thread a correction is waiting on.
    const IO_TIMEOUT: Duration = Duration::from_millis(250);

    fn socket_path() -> Option<&'static PathBuf> {
        static PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
        PATH.get_or_init(|| {
            let sig = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok()?;
            // Hyprland moved this under XDG_RUNTIME_DIR in 0.40; older builds
            // kept it in /tmp. Both are checked so the daemon works either way.
            let runtime = std::env::var("XDG_RUNTIME_DIR").ok().map(PathBuf::from);
            [
                runtime.map(|r| r.join("hypr").join(&sig).join(".socket.sock")),
                Some(PathBuf::from("/tmp/hypr").join(&sig).join(".socket.sock")),
            ]
            .into_iter()
            .flatten()
            .find(|p| p.exists())
        })
        .as_ref()
    }

    /// Send one command and return the reply, or `None` if this is not a
    /// Hyprland session or the socket would not answer.
    pub fn request(command: &str) -> Option<String> {
        let mut stream = UnixStream::connect(socket_path()?).ok()?;
        let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
        let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
        stream.write_all(command.as_bytes()).ok()?;
        let mut reply = String::new();
        stream.read_to_string(&mut reply).ok()?;
        Some(reply)
    }

    /// Whether this session is one we can drive at all.
    pub fn available() -> bool {
        socket_path().is_some()
    }

    fn devices() -> Option<String> {
        match request("j/devices") {
            Some(reply) => Some(reply),
            // The socket refused. Fall back to the subprocess so a setup that
            // only has `hyprctl` still works.
            None => {
                let out = std::process::Command::new("hyprctl")
                    .args(["devices", "-j"])
                    .output()
                    .ok()?;
                String::from_utf8(out.stdout).ok()
            }
        }
    }

    /// Each keyboard block of a `j/devices` reply, in order, with our own
    /// injector left out.
    ///
    /// Splitting on `{` rather than parsing is enough because the blocks are
    /// flat, and stopping at the end of the keyboards array is what keeps the
    /// mice and tablets below it out of the answer.
    fn keyboards(devices_json: &str) -> impl Iterator<Item = &str> {
        let keyboards = devices_json
            .find("\"keyboards\"")
            .map(|at| &devices_json[at..])
            .unwrap_or("");
        keyboards
            .split('{')
            .skip(1)
            .take_while(|block| !block.starts_with(']'))
            .filter(|block| json_str(block, "name") != Some(INJECTOR))
    }

    /// The active layout, read out of a `j/devices` reply.
    ///
    /// Deliberately not "whatever keyboard is flagged `main`". On a session with
    /// an input method running, `main` is the *input method's own virtual
    /// keyboard* — on the machine this was written on,
    /// `hl-virtual-keyboard-fcitx5`. Reading it means reporting the layout of a
    /// device nobody types on.
    ///
    /// So: our injector never counts, `main` is preferred among the rest, and
    /// anything else recognizable is the fallback.
    pub fn parse_layout(devices_json: &str) -> Option<Language> {
        let mut fallback = None;
        for block in keyboards(devices_json) {
            let Some(lang) = json_str(block, "active_keymap").and_then(language_of_keymap) else {
                continue;
            };
            let is_main = block.contains("\"main\": true") || block.contains("\"main\":true");
            if is_main {
                return Some(lang);
            }
            fallback.get_or_insert(lang);
        }
        fallback
    }

    /// The configured layout list, as the comma-separated xkb codes Hyprland
    /// reports per device (`"layout": "us,il"`).
    ///
    /// Taken from the device with the *longest* list rather than from `main`,
    /// for the same reason `parse_layout` distrusts `main`: a virtual keyboard
    /// often carries a single-layout list of its own, and reading `us` off it
    /// would make Hebrew unreachable.
    pub fn parse_names(devices_json: &str) -> Option<Vec<String>> {
        keyboards(devices_json)
            .filter_map(|block| json_str(block, "layout"))
            .map(|layout| {
                layout
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .collect::<Vec<_>>()
            })
            .max_by_key(Vec::len)
    }

    pub fn current() -> Option<Language> {
        parse_layout(&devices()?)
    }

    pub fn names() -> Option<Vec<String>> {
        parse_names(&devices()?)
    }

    /// Whether an event-socket line is a layout change on a keyboard the user
    /// types on.
    ///
    /// The line is `activelayout>>KEYBOARD,LAYOUT`, and our own injector emits
    /// one of these every time a correction switches the layout — which is a
    /// change we made rather than one to react to, and already in the cache.
    pub fn is_layout_event(line: &str) -> bool {
        line.starts_with("activelayout>>") && !line.contains(INJECTOR)
    }

    /// Hyprland's second socket, which pushes events rather than answering
    /// questions. Same directory as the control socket, so it is found by the
    /// same probe.
    pub fn subscribe() -> Option<super::Signals> {
        let path = socket_path()?.parent()?.join(".socket2.sock");
        // No read timeout, unlike `request`: waiting is the whole job here, and
        // a timeout would end the stream on the first quiet quarter-second.
        let stream = UnixStream::connect(path).ok()?;
        Some(super::Lines::stream(
            std::io::BufReader::new(stream),
            is_layout_event,
            None,
        ))
    }

    pub fn set_index(index: usize) -> bool {
        if available() {
            // Hyprland answers "ok" and nothing else on success.
            return request(&format!("/switchxkblayout all {index}"))
                .is_some_and(|reply| reply.trim() == "ok");
        }
        match std::process::Command::new("hyprctl")
            .args(["switchxkblayout", "all", &index.to_string()])
            .status()
        {
            Ok(status) => status.success(),
            Err(e) => {
                eprintln!("Failed to switch layout using hyprctl: {e}");
                false
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// sway — the i3 IPC socket
// ─────────────────────────────────────────────────────────────────────────────

/// sway (and anything else speaking i3's IPC) over `$SWAYSOCK`.
///
/// The protocol is small enough to speak directly, and worth speaking directly
/// for the same reason Hyprland's is: `swaymsg` is a process launch per query,
/// on the thread a correction is waiting on. A message is the six bytes
/// `i3-ipc`, a little-endian payload length, a little-endian type, then the
/// payload; the reply has the same shape.
mod sway {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    use std::path::PathBuf;
    use std::sync::OnceLock;
    use std::time::Duration;

    use super::{json_num, json_str, json_str_array, INJECTOR};

    const MAGIC: &[u8; 6] = b"i3-ipc";
    const RUN_COMMAND: u32 = 0;
    const SUBSCRIBE: u32 = 2;
    const GET_INPUTS: u32 = 100;
    const IO_TIMEOUT: Duration = Duration::from_millis(250);

    fn socket_path() -> Option<&'static PathBuf> {
        static PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
        PATH.get_or_init(|| {
            // SWAYSOCK is what sway exports; I3SOCK is the same protocol under
            // i3, which has no xkb layout switching of its own but does answer
            // the input query.
            let sock = std::env::var("SWAYSOCK")
                .or_else(|_| std::env::var("I3SOCK"))
                .ok()?;
            let path = PathBuf::from(sock);
            path.exists().then_some(path)
        })
        .as_ref()
    }

    pub fn available() -> bool {
        socket_path().is_some()
    }

    fn send(stream: &mut UnixStream, kind: u32, payload: &str) -> Option<()> {
        let mut message = Vec::with_capacity(14 + payload.len());
        message.extend_from_slice(MAGIC);
        message.extend_from_slice(&(payload.len() as u32).to_ne_bytes());
        message.extend_from_slice(&kind.to_ne_bytes());
        message.extend_from_slice(payload.as_bytes());
        stream.write_all(&message).ok()
    }

    /// Read one whole message off the stream.
    ///
    /// The header is fixed-width and the length in it is what says how much more
    /// to read — a `read_to_string` here would block until sway closed the
    /// connection, which it does not do between messages. Shared with the event
    /// stream, where that property is the point rather than a hazard.
    fn recv(stream: &mut UnixStream) -> Option<String> {
        let mut header = [0u8; 14];
        stream.read_exact(&mut header).ok()?;
        if &header[..6] != MAGIC {
            return None;
        }
        let len = u32::from_ne_bytes(header[6..10].try_into().ok()?) as usize;
        let mut body = vec![0u8; len];
        stream.read_exact(&mut body).ok()?;
        String::from_utf8(body).ok()
    }

    fn request(kind: u32, payload: &str) -> Option<String> {
        let mut stream = UnixStream::connect(socket_path()?).ok()?;
        let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
        let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
        send(&mut stream, kind, payload)?;
        recv(&mut stream)
    }

    /// A connection of its own, subscribed to input events.
    ///
    /// It has to be a second connection: this one blocks forever waiting to be
    /// told something, and the one `request` uses has a timeout because a
    /// correction is waiting on it.
    pub fn subscribe() -> Option<super::Signals> {
        let mut stream = UnixStream::connect(socket_path()?).ok()?;
        send(&mut stream, SUBSCRIBE, "[\"input\"]")?;
        // sway replies to the subscription itself before any event; reading it
        // here keeps the iterator's messages aligned with actual events.
        let reply = recv(&mut stream)?;
        if !reply.contains("\"success\": true") && !reply.contains("\"success\":true") {
            return None;
        }
        Some(Box::new(Events { stream }))
    }

    /// Input events, narrowed to the ones that are layout changes.
    ///
    /// Everything about a keyboard or a pointer arrives on this subscription —
    /// devices added and removed, pointer settings, key repeat rates — and the
    /// payload names which it is.
    struct Events {
        stream: UnixStream,
    }

    impl Iterator for Events {
        type Item = ();

        fn next(&mut self) -> Option<()> {
            loop {
                let body = recv(&mut self.stream)?;
                if body.contains("xkb_layout") {
                    return Some(());
                }
            }
        }
    }

    /// The keyboard entries of a `GET_INPUTS` reply, our own injector left out.
    fn keyboards(inputs_json: &str) -> impl Iterator<Item = &str> {
        inputs_json
            .split('{')
            .skip(1)
            .filter(|block| json_str(block, "type") == Some("keyboard"))
            .filter(|block| json_str(block, "identifier") != Some(INJECTOR))
            .filter(|block| json_str(block, "name") != Some(INJECTOR))
    }

    /// The configured layouts, from the keyboard that lists the most of them —
    /// a virtual device with a single-layout list must not be what decides
    /// Hebrew is unavailable.
    pub fn parse_names(inputs_json: &str) -> Option<Vec<String>> {
        keyboards(inputs_json)
            .filter_map(|block| json_str_array(block, "xkb_layout_names"))
            .max_by_key(Vec::len)
    }

    /// Which layout is live, as an index into [`parse_names`].
    ///
    /// Read off the same keyboard the names came from: a device with its own
    /// shorter list indexes into that list, not into this one.
    pub fn parse_index(inputs_json: &str) -> Option<usize> {
        keyboards(inputs_json)
            .filter_map(|block| {
                let names = json_str_array(block, "xkb_layout_names")?;
                let index = json_num(block, "xkb_active_layout_index")?;
                Some((names.len(), index))
            })
            .max_by_key(|(len, _)| *len)
            .map(|(_, index)| index)
    }

    fn inputs() -> Option<String> {
        request(GET_INPUTS, "")
    }

    pub fn names() -> Option<Vec<String>> {
        parse_names(&inputs()?)
    }

    pub fn current_index() -> Option<usize> {
        parse_index(&inputs()?)
    }

    pub fn set_index(index: usize) -> bool {
        // `type:keyboard` rather than a device identifier: sway applies it to
        // every keyboard, which is what "switch the layout" means to a user
        // with a laptop keyboard and an external one plugged in.
        let reply = request(
            RUN_COMMAND,
            &format!("input type:keyboard xkb_switch_layout {index}"),
        );
        // The reply is `[{"success": true}]`, or the same with a `parse_error`.
        reply.is_some_and(|r| r.contains("\"success\": true") || r.contains("\"success\":true"))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// KDE Plasma — org.kde.KeyboardLayouts over D-Bus
// ─────────────────────────────────────────────────────────────────────────────

/// KDE keeps its layout state behind `org.kde.KeyboardLayouts` on the session
/// bus, which is the only way to drive it under Plasma Wayland and the correct
/// way under Plasma X11 (setting the XKB group directly works until KWin's own
/// layout policy sets it back).
///
/// Reached through whichever D-Bus command-line client is installed rather than
/// by speaking D-Bus: the wire protocol needs a SASL handshake and a type
/// system, which is a large amount of code to reach two methods. `gdbus` ships
/// with GLib, `busctl` with systemd, `qdbus` with Qt — a Plasma session has at
/// least one, in practice all three.
mod kde {
    use std::process::Command;
    use std::sync::OnceLock;

    /// The first D-Bus client on this system that can call a method, found once.
    fn client() -> Option<&'static (&'static str, Style)> {
        static CLIENT: OnceLock<Option<(&'static str, Style)>> = OnceLock::new();
        CLIENT
            .get_or_init(|| {
                for (bin, style) in [
                    ("gdbus", Style::Gdbus),
                    ("busctl", Style::Busctl),
                    ("qdbus6", Style::Qdbus),
                    ("qdbus", Style::Qdbus),
                ] {
                    if Command::new(bin)
                        .arg("--version")
                        .output()
                        .is_ok_and(|o| o.status.success())
                    {
                        return Some((bin, style));
                    }
                }
                None
            })
            .as_ref()
    }

    #[derive(Clone, Copy)]
    pub enum Style {
        Gdbus,
        Busctl,
        Qdbus,
    }

    /// Call one method on `/Layouts` and return stdout, whatever client is
    /// available. Arguments are only ever a single unsigned integer here, which
    /// is why this takes one rather than a general argument list.
    fn call(method: &str, arg: Option<usize>) -> Option<String> {
        let (bin, style) = *client()?;
        let mut cmd = Command::new(bin);
        match style {
            Style::Gdbus => {
                cmd.args([
                    "call",
                    "--session",
                    "--dest",
                    "org.kde.keyboard",
                    "--object-path",
                    "/Layouts",
                    "--method",
                    &format!("org.kde.KeyboardLayouts.{method}"),
                ]);
                if let Some(n) = arg {
                    cmd.arg(format!("{n}"));
                }
            }
            Style::Busctl => {
                cmd.args([
                    "--user",
                    "call",
                    "org.kde.keyboard",
                    "/Layouts",
                    "org.kde.KeyboardLayouts",
                    method,
                ]);
                match arg {
                    Some(n) => {
                        cmd.args(["u", &n.to_string()]);
                    }
                    None => {
                        cmd.arg("");
                    }
                }
            }
            Style::Qdbus => {
                cmd.args(["org.kde.keyboard", "/Layouts", &format!("org.kde.KeyboardLayouts.{method}")]);
                if let Some(n) = arg {
                    cmd.arg(n.to_string());
                }
            }
        }
        let out = cmd.output().ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// The display names out of a `getLayoutsList` reply.
    ///
    /// Each entry is a `(shortName, displayName, longName)` triple —
    /// `('us', '', 'English (US)')` — and the long name is the one worth
    /// matching, because the short name is missing on some entries and the
    /// display name is the two-letter badge KDE draws in the tray. Every
    /// client's output is quoted differently, so the shared shape is what is
    /// parsed: quoted strings in order, three per layout.
    pub fn parse_names(reply: &str) -> Option<Vec<String>> {
        let quoted = quoted_strings(reply);
        (quoted.len() >= 3 && quoted.len().is_multiple_of(3))
            .then(|| quoted.chunks(3).map(|t| t[2].clone()).collect())
    }

    /// Every quoted string in a reply, in order, single or double quotes.
    fn quoted_strings(reply: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut chars = reply.chars().peekable();
        while let Some(c) = chars.next() {
            if c != '\'' && c != '"' {
                continue;
            }
            let mut value = String::new();
            for inner in chars.by_ref() {
                if inner == c {
                    break;
                }
                value.push(inner);
            }
            out.push(value);
        }
        out
    }

    /// The first bare integer in a reply — `(uint32 1,)`, `u 1`, or `1`.
    pub fn parse_index(reply: &str) -> Option<usize> {
        reply
            .split(|c: char| !c.is_ascii_digit())
            .find(|t| !t.is_empty())
            .and_then(|t| t.parse().ok())
    }

    pub fn names() -> Option<Vec<String>> {
        parse_names(&call("getLayoutsList", None)?)
    }

    /// Plasma emits `layoutChanged` on the same interface it answers questions
    /// on, so the change can be followed instead of polled for.
    ///
    /// `gdbus` only. `busctl` and `qdbus` can both call a method, which is why
    /// they are accepted above, but neither prints a signal stream in a shape
    /// worth parsing — and a session that has one of them and not gdbus simply
    /// keeps the cache it had before.
    pub fn subscribe() -> Option<super::Signals> {
        let (bin, style) = *client()?;
        if !matches!(style, Style::Gdbus) {
            return None;
        }
        super::watch_command(
            bin,
            &["monitor", "--session", "--dest", "org.kde.keyboard"],
            |line| line.contains("layoutChanged"),
        )
    }

    pub fn current_index() -> Option<usize> {
        // `getLayout` returns the index; `uint32` in the reply would be read as
        // the number by a naive scan, which is why the type prefix is stripped
        // before the digits are looked for.
        let reply = call("getLayout", None)?;
        parse_index(&reply.replace("uint32", " ").replace("u32", " "))
    }

    pub fn set_index(index: usize) -> bool {
        // `setLayout` answers with a boolean; anything that is not an explicit
        // false is taken as accepted, since the poll in `switch_layout_to` is
        // what actually decides whether it landed.
        call("setLayout", Some(index)).is_some_and(|r| !r.contains("false"))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// GNOME — org.gnome.desktop.input-sources
// ─────────────────────────────────────────────────────────────────────────────

/// GNOME manages input sources itself, above XKB, on both Wayland and X11: the
/// list is `sources` and the live one is the index in `current`. Writing
/// `current` is what gnome-shell watches, so it is a supported switch rather
/// than a trick — the XKB group underneath is set back by the shell within a
/// keystroke.
///
/// `gsettings` is part of GLib and therefore of GNOME itself, so unlike the KDE
/// backend there is no client to choose between.
mod gnome {
    use std::process::Command;

    fn get(key: &str) -> Option<String> {
        let out = Command::new("gsettings")
            .args(["get", "org.gnome.desktop.input-sources", key])
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// The xkb codes out of a `sources` reply.
    ///
    /// The value is `[('xkb', 'us'), ('xkb', 'il')]`, and an IBus entry —
    /// `('ibus', 'mozc-jp')` — is a real input source that is not an xkb layout.
    /// It is kept in the list rather than dropped, because `current` is an index
    /// into the whole thing and skipping an entry would shift every index after
    /// it; `language_of_keymap` returns `None` for it, which is the right answer.
    pub fn parse_names(reply: &str) -> Option<Vec<String>> {
        let mut out = Vec::new();
        for pair in reply.split("('").skip(1) {
            let inner: Vec<&str> = pair.split('\'').collect();
            // ["xkb", ", ", "us", ...] — the source's own code is the third.
            if let Some(code) = inner.get(2) {
                out.push(code.to_string());
            }
        }
        (!out.is_empty()).then_some(out)
    }

    pub fn names() -> Option<Vec<String>> {
        parse_names(&get("sources")?)
    }

    /// `gsettings monitor` prints a line every time the key is written, and
    /// keeps running until it is killed — one process for the life of the
    /// daemon, in place of one per word that misses the cache.
    pub fn subscribe() -> Option<super::Signals> {
        super::watch_command(
            "gsettings",
            &["monitor", "org.gnome.desktop.input-sources", "current"],
            // Every line is about the key we asked to be told about, so the only
            // thing to exclude is the blank one at the end.
            |line| !line.trim().is_empty(),
        )
    }

    pub fn current_index() -> Option<usize> {
        // `uint32 1`
        get("current")?
            .split_whitespace()
            .last()
            .and_then(|t| t.parse().ok())
    }

    pub fn set_index(index: usize) -> bool {
        Command::new("gsettings")
            .args([
                "set",
                "org.gnome.desktop.input-sources",
                "current",
                &format!("uint32 {index}"),
            ])
            .status()
            .is_ok_and(|s| s.success())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// X11 — the XKB group
// ─────────────────────────────────────────────────────────────────────────────

/// Any X11 session, whatever is drawing the windows.
///
/// This is the backend that covers the long tail: i3, xfce, MATE, Cinnamon,
/// openbox, and anything else that leaves the keymap to the X server. The group
/// is read and set with two Xlib calls, so it costs a round trip rather than a
/// process, and the layout list comes out of the `_XKB_RULES_NAMES` property on
/// the root window — the same place `setxkbmap -query` reads it from.
///
/// `libX11` is loaded with `dlopen` rather than linked. Linking it would make
/// the binary refuse to start on a pure Wayland system that has no X libraries
/// installed, to provide a backend such a system cannot use anyway.
mod x11 {
    use std::ffi::{c_char, c_int, c_long, c_ulong, c_void, CString};
    use std::sync::{Mutex, OnceLock};

    use nix::libc::{dlopen, dlsym, RTLD_LAZY};

    /// `XkbUseCoreKbd` — the keyboard the group applies to.
    const XKB_USE_CORE_KBD: u32 = 0x0100;

    type Display = c_void;
    type Window = c_ulong;
    type Atom = c_ulong;
    type Bool = c_int;

    /// The subset of `XkbStateRec` up to the field this needs. Xlib writes the
    /// whole struct, so the buffer handed to it has to be the whole size — the
    /// tail is padding here rather than a struct nobody reads.
    #[repr(C)]
    #[derive(Default)]
    struct XkbState {
        group: u8,
        locked_group: u8,
        base_group: u16,
        latched_group: u16,
        mods: u8,
        base_mods: u8,
        latched_mods: u8,
        locked_mods: u8,
        compat_state: u8,
        grab_mods: u8,
        compat_grab_mods: u8,
        lookup_mods: u8,
        compat_lookup_mods: u8,
        ptr_buttons: u16,
        // Xlib's struct is longer than the fields above; the padding keeps the
        // write inside our allocation.
        _tail: [u8; 16],
    }

    // Named individually because `dlsym` hands back a `void*` and the cast to a
    // function pointer has to state the signature it is casting to — an
    // unannotated `transmute` here would compile whatever the symbol's real
    // shape, and be wrong at the call rather than at the cast.
    type XOpenDisplayFn = unsafe extern "C" fn(*const c_char) -> *mut Display;
    type XDefaultRootWindowFn = unsafe extern "C" fn(*mut Display) -> Window;
    type XInternAtomFn = unsafe extern "C" fn(*mut Display, *const c_char, Bool) -> Atom;
    type XGetWindowPropertyFn = unsafe extern "C" fn(
        *mut Display,
        Window,
        Atom,
        c_long,
        c_long,
        Bool,
        Atom,
        *mut Atom,
        *mut c_int,
        *mut c_ulong,
        *mut c_ulong,
        *mut *mut u8,
    ) -> c_int;
    type XFreeFn = unsafe extern "C" fn(*mut c_void) -> c_int;
    type XkbGetStateFn = unsafe extern "C" fn(*mut Display, u32, *mut XkbState) -> c_int;
    type XkbLockGroupFn = unsafe extern "C" fn(*mut Display, u32, u32) -> Bool;
    type XFlushFn = unsafe extern "C" fn(*mut Display) -> c_int;

    struct Lib {
        open_display: XOpenDisplayFn,
        default_root_window: XDefaultRootWindowFn,
        intern_atom: XInternAtomFn,
        get_window_property: XGetWindowPropertyFn,
        free: XFreeFn,
        xkb_get_state: XkbGetStateFn,
        xkb_lock_group: XkbLockGroupFn,
        flush: XFlushFn,
    }

    /// The X connection, opened once and kept. `Display*` is not thread-safe,
    /// so it lives behind a mutex; the calls under it are microseconds.
    struct Conn {
        lib: Lib,
        display: *mut Display,
    }

    // The pointer is only ever dereferenced through the Mutex below, which is
    // what makes the Xlib calls serialised — the requirement Xlib actually has.
    unsafe impl Send for Conn {}

    fn conn() -> Option<&'static Mutex<Conn>> {
        static CONN: OnceLock<Option<Mutex<Conn>>> = OnceLock::new();
        CONN.get_or_init(|| unsafe {
            // No DISPLAY means no X server to talk to, and dlopen'ing the
            // library to find that out would be work for nothing.
            std::env::var("DISPLAY").ok()?;
            let name = CString::new("libX11.so.6").ok()?;
            let mut handle = dlopen(name.as_ptr(), RTLD_LAZY);
            if handle.is_null() {
                // Some distributions ship only the unversioned development
                // symlink; it is the same library.
                let alt = CString::new("libX11.so").ok()?;
                handle = dlopen(alt.as_ptr(), RTLD_LAZY);
            }
            if handle.is_null() {
                return None;
            }
            macro_rules! sym {
                ($name:literal, $ty:ty) => {{
                    let s = CString::new($name).ok()?;
                    let p = dlsym(handle, s.as_ptr());
                    if p.is_null() {
                        return None;
                    }
                    std::mem::transmute::<*mut c_void, $ty>(p)
                }};
            }
            let lib = Lib {
                open_display: sym!("XOpenDisplay", XOpenDisplayFn),
                default_root_window: sym!("XDefaultRootWindow", XDefaultRootWindowFn),
                intern_atom: sym!("XInternAtom", XInternAtomFn),
                get_window_property: sym!("XGetWindowProperty", XGetWindowPropertyFn),
                free: sym!("XFree", XFreeFn),
                xkb_get_state: sym!("XkbGetState", XkbGetStateFn),
                xkb_lock_group: sym!("XkbLockGroup", XkbLockGroupFn),
                flush: sym!("XFlush", XFlushFn),
            };
            let display = (lib.open_display)(std::ptr::null());
            if display.is_null() {
                return None;
            }
            Some(Mutex::new(Conn { lib, display }))
        })
        .as_ref()
    }

    pub fn available() -> bool {
        conn().is_some()
    }

    /// The configured layouts, from `_XKB_RULES_NAMES` on the root window.
    ///
    /// The property is five NUL-separated strings — rules, model, layout,
    /// variant, options — and the third is the comma-separated layout list this
    /// wants. It is what `setxkbmap -query` prints, read without the process.
    pub fn names() -> Option<Vec<String>> {
        let guard = conn()?.lock().ok()?;
        let c = &*guard;
        unsafe {
            let prop = CString::new("_XKB_RULES_NAMES").ok()?;
            let atom = (c.lib.intern_atom)(c.display, prop.as_ptr(), 0);
            if atom == 0 {
                return None;
            }
            let root = (c.lib.default_root_window)(c.display);
            let mut actual_type: Atom = 0;
            let mut actual_format: c_int = 0;
            let mut nitems: c_ulong = 0;
            let mut bytes_after: c_ulong = 0;
            let mut data: *mut u8 = std::ptr::null_mut();
            // 1024 32-bit words is far more than the property ever holds, and
            // asking for the whole thing at once avoids a second round trip to
            // read the remainder.
            let status = (c.lib.get_window_property)(
                c.display,
                root,
                atom,
                0,
                1024,
                0,
                0, // AnyPropertyType
                &mut actual_type,
                &mut actual_format,
                &mut nitems,
                &mut bytes_after,
                &mut data,
            );
            if status != 0 || data.is_null() || nitems == 0 {
                return None;
            }
            let bytes = std::slice::from_raw_parts(data, nitems as usize);
            let fields: Vec<String> = bytes
                .split(|b| *b == 0)
                .map(|f| String::from_utf8_lossy(f).into_owned())
                .collect();
            (c.lib.free)(data as *mut c_void);
            // rules, model, layout, …
            let layouts = fields.get(2)?;
            let list: Vec<String> = layouts
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            (!list.is_empty()).then_some(list)
        }
    }

    pub fn current_index() -> Option<usize> {
        let guard = conn()?.lock().ok()?;
        let c = &*guard;
        unsafe {
            let mut state = XkbState::default();
            // Returns Success (0) on success, like the rest of Xlib.
            if (c.lib.xkb_get_state)(c.display, XKB_USE_CORE_KBD, &mut state) != 0 {
                return None;
            }
            Some(state.group as usize)
        }
    }

    pub fn set_index(index: usize) -> bool {
        let Some(lock) = conn() else { return false };
        let Ok(guard) = lock.lock() else { return false };
        let c = &*guard;
        unsafe {
            let ok = (c.lib.xkb_lock_group)(c.display, XKB_USE_CORE_KBD, index as u32) != 0;
            // Xlib buffers requests; without this the group changes whenever
            // something else happens to flush, which is not a timescale a
            // correction can wait on.
            (c.lib.flush)(c.display);
            ok
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Hyprland ─────────────────────────────────────────────────────────────

    /// Trimmed from a real `j/devices` reply on a Hyprland session running
    /// fcitx5, which is where this bug was found: the keyboard flagged `main`
    /// is the input method's virtual one, and ReCast's own injector is in the
    /// list beside it.
    const DEVICES: &str = r#"{"mice": [], "keyboards": [
        {"address": "0x1", "name": "at-translated-set-2-keyboard", "rules": "",
         "model": "", "layout": "us,il", "variant": "", "options": "",
         "active_keymap": "Hebrew", "capsLock": false, "numLock": false, "main": false},
        {"address": "0x2", "name": "recast-injector", "rules": "",
         "model": "", "layout": "us", "variant": "", "options": "",
         "active_keymap": "English (US)", "capsLock": false, "numLock": false, "main": false},
        {"address": "0x3", "name": "hl-virtual-keyboard-fcitx5", "rules": "",
         "model": "", "layout": "us,il", "variant": "", "options": "",
         "active_keymap": "Hebrew", "capsLock": false, "numLock": false, "main": true}
    ], "tablets": []}"#;

    #[test]
    fn the_layout_comes_from_a_keyboard_someone_types_on() {
        assert_eq!(hypr::parse_layout(DEVICES), Some(Language::Hebrew));
    }

    #[test]
    fn our_own_injector_never_decides_the_answer() {
        // The injector says English while every real keyboard says Hebrew. If
        // it were allowed to win, `switch_layout_to` would believe a switch to
        // English was unnecessary and the correction would be typed out in
        // Hebrew — the intermittent garbage this was reported as.
        let only_injector_is_main = DEVICES
            .replace(r#""name": "recast-injector"#, r#""name": "recast-injector-x"#)
            .replace(r#""name": "hl-virtual-keyboard-fcitx5", "rules": "",
         "model": "", "layout": "us,il", "variant": "", "options": "",
         "active_keymap": "Hebrew", "capsLock": false, "numLock": false, "main": true"#,
                     r#""name": "recast-injector", "rules": "",
         "model": "", "layout": "us", "variant": "", "options": "",
         "active_keymap": "English (US)", "capsLock": false, "numLock": false, "main": true"#);
        // With our injector as `main` and claiming English, the answer must
        // still come from the physical keyboard.
        assert_eq!(
            hypr::parse_layout(&only_injector_is_main),
            Some(Language::Hebrew)
        );
    }

    #[test]
    fn a_reply_with_nothing_recognizable_is_not_guessed_at() {
        assert_eq!(hypr::parse_layout("{}"), None);
        assert_eq!(hypr::parse_layout(r#"{"keyboards": []}"#), None);
    }

    /// The list the index is looked up in. Our injector carries a single-layout
    /// list of its own, and taking *that* as the configured set is what would
    /// make Hebrew unreachable — the failure the "longest list wins" rule is
    /// there to avoid.
    #[test]
    fn the_hyprland_layout_list_ignores_the_injectors_own() {
        assert_eq!(
            hypr::parse_names(DEVICES),
            Some(vec!["us".to_string(), "il".to_string()])
        );
    }

    #[test]
    fn only_a_layout_change_on_a_real_keyboard_wakes_the_watcher() {
        // The event socket carries every kind of event Hyprland emits; the
        // watcher re-queries on each signal, so anything else is a wasted query.
        assert!(hypr::is_layout_event(
            "activelayout>>at-translated-set-2-keyboard,Hebrew"
        ));
        assert!(!hypr::is_layout_event("workspace>>2"));
        assert!(!hypr::is_layout_event("openwindow>>a1b2,1,kitty,shell"));
        // Our own injector switching layouts is a change we just made, and it
        // is already in the cache.
        assert!(!hypr::is_layout_event(&format!(
            "activelayout>>{INJECTOR},Hebrew"
        )));
    }

    // ── sway ─────────────────────────────────────────────────────────────────

    /// Trimmed from a real `swaymsg -t get_inputs` reply.
    const INPUTS: &str = r#"[
      {"identifier": "1:1:AT_Translated_Set_2_keyboard", "name": "AT keyboard",
       "type": "keyboard", "xkb_active_layout_index": 1,
       "xkb_layout_names": ["English (US)", "Hebrew"],
       "xkb_active_layout_name": "Hebrew"},
      {"identifier": "recast-injector", "name": "recast-injector",
       "type": "keyboard", "xkb_active_layout_index": 0,
       "xkb_layout_names": ["English (US)"]},
      {"identifier": "2:2:mouse", "name": "a mouse", "type": "pointer"}
    ]"#;

    #[test]
    fn sway_reports_its_layouts_and_which_one_is_live() {
        assert_eq!(
            sway::parse_names(INPUTS),
            Some(vec!["English (US)".to_string(), "Hebrew".to_string()])
        );
        assert_eq!(sway::parse_index(INPUTS), Some(1));
    }

    #[test]
    fn the_swayd_injector_is_left_out_of_both_answers() {
        // It is a keyboard, it is first in nothing, and it claims one layout
        // and index 0 — exactly the shape that would answer both questions
        // wrongly if it were counted.
        let names = sway::parse_names(INPUTS).unwrap();
        assert_eq!(names.len(), 2, "the injector's single layout was counted");
    }

    // ── KDE ──────────────────────────────────────────────────────────────────

    #[test]
    fn kde_layout_names_come_out_of_any_clients_quoting() {
        // gdbus
        let gdbus = "([('us', '', 'English (US)'), ('il', '', 'Hebrew')],)";
        assert_eq!(
            kde::parse_names(gdbus),
            Some(vec!["English (US)".to_string(), "Hebrew".to_string()])
        );
        // busctl, which double-quotes and prefixes the count
        let busctl = r#"a(sss) 2 "us" "" "English (US)" "il" "" "Hebrew""#;
        assert_eq!(
            kde::parse_names(busctl),
            Some(vec!["English (US)".to_string(), "Hebrew".to_string()])
        );
    }

    #[test]
    fn kde_reports_the_live_layout_as_an_index() {
        assert_eq!(kde::parse_index("(uint32 1,)".replace("uint32", " ").as_str()), Some(1));
        assert_eq!(kde::parse_index("0"), Some(0));
    }

    // ── GNOME ────────────────────────────────────────────────────────────────

    #[test]
    fn gnome_input_sources_keep_their_positions() {
        // An IBus entry is not an xkb layout, but it still occupies an index —
        // dropping it would shift every layout after it and switch the user to
        // the wrong one.
        let sources = "[('xkb', 'us'), ('ibus', 'mozc-jp'), ('xkb', 'il')]";
        let names = gnome::parse_names(sources).unwrap();
        assert_eq!(names, vec!["us", "mozc-jp", "il"]);
        assert_eq!(
            names
                .iter()
                .position(|n| language_of_keymap(n) == Some(Language::Hebrew)),
            Some(2),
            "Hebrew is at index 2, not 1"
        );
    }

    // ── shared ───────────────────────────────────────────────────────────────

    /// The bug the whole index lookup exists for.
    #[test]
    fn the_hebrew_index_is_looked_up_not_assumed() {
        let hebrew_first = ["il", "us"];
        let hebrew_third = ["us", "ru", "il"];
        let find = |list: &[&str], lang| {
            list.iter()
                .position(|n| language_of_keymap(n) == Some(lang))
        };
        assert_eq!(find(&hebrew_first, Language::Hebrew), Some(0));
        assert_eq!(find(&hebrew_third, Language::Hebrew), Some(2));
        // The old code sent `switchxkblayout all 1` in every one of these
        // cases, which on the third list is Russian.
        assert_eq!(find(&hebrew_third, Language::English), Some(0));
    }

    #[test]
    fn json_helpers_read_what_the_compositors_send() {
        let block = r#""name": "kbd", "index": 12, "list": ["a", "b c"], "flag": true"#;
        assert_eq!(json_str(block, "name"), Some("kbd"));
        assert_eq!(json_num(block, "index"), Some(12));
        assert_eq!(
            json_str_array(block, "list"),
            Some(vec!["a".to_string(), "b c".to_string()])
        );
        assert_eq!(json_str(block, "absent"), None);
        assert_eq!(json_num(block, "absent"), None);
    }

    /// Against the session that is actually running, when it is one we can
    /// drive. Skips otherwise — CI has no desktop — but on a developer's
    /// machine it is the only thing that checks the socket paths, the protocols
    /// and the parsers against reality rather than against fixtures.
    #[test]
    fn the_live_session_answers_if_there_is_one() {
        let b = backend();
        eprintln!("backend: {}", describe_backend());
        if b == Backend::None {
            eprintln!("no layout backend here — skipping the live check");
            return;
        }
        let names = b.names();
        assert!(
            names.as_ref().is_some_and(|n| !n.is_empty()),
            "{b:?} is available but reports no layouts"
        );
        // And the cached front door agrees with the raw read, when the session
        // is on a layout ReCast knows.
        if let Some(live) = b.current() {
            assert_eq!(query_layout(), Some(live));
        }
    }
}
