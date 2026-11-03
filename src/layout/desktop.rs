//! Optional AT-SPI adapter, isolated from the keyboard process and bounded by deadlines.
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct Helper {
    child: Child,
    input: ChildStdin,
    output: ChildStdout,
    sequence: u64,
}
impl Drop for Helper {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Helper {
    fn start() -> Option<Self> {
        let mut child = Command::new("python3")
            .args([
                "-I",
                "-u",
                "-c",
                include_str!("../../scripts/linux_desktop.py"),
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let input = child.stdin.take()?;
        let output = child.stdout.take()?;
        let mut helper = Self {
            child,
            input,
            output,
            sequence: 0,
        };
        use nix::fcntl::{fcntl, FcntlArg, OFlag};
        fcntl(
            helper.input.as_raw_fd(),
            FcntlArg::F_SETFL(OFlag::O_NONBLOCK),
        )
        .ok()?;
        fcntl(
            helper.output.as_raw_fd(),
            FcntlArg::F_SETFL(OFlag::O_NONBLOCK),
        )
        .ok()?;
        helper
            .read(Instant::now() + Duration::from_secs(2))?
            .get("ready")?
            .as_bool()?
            .then_some(helper)
    }
    fn read(&mut self, deadline: Instant) -> Option<Value> {
        let mut bytes = Vec::new();
        let mut buffer = [0; 4096];
        while Instant::now() < deadline {
            match self.output.read(&mut buffer) {
                Ok(0) => return None,
                Ok(count) => {
                    bytes.extend_from_slice(&buffer[..count]);
                    if bytes.len() > 512 * 1024 {
                        return None;
                    }
                    if bytes.last() == Some(&b'\n') {
                        return serde_json::from_slice(&bytes).ok();
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(1))
                }
                Err(_) => return None,
            }
        }
        None
    }
    fn request(&mut self, mut request: Value, timeout: Duration) -> Option<Value> {
        self.sequence = self.sequence.wrapping_add(1);
        request["sequence"] = json!(self.sequence);
        request["deadline"] = json!(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()?
                .as_secs_f64()
                + timeout.as_secs_f64()
        );
        let mut bytes = serde_json::to_vec(&request).ok()?;
        if bytes.len() > 512 * 1024 {
            return None;
        }
        bytes.push(b'\n');
        let deadline = Instant::now() + timeout;
        let mut remaining = bytes.as_slice();
        while !remaining.is_empty() && Instant::now() < deadline {
            match self.input.write(remaining) {
                Ok(0) => return None,
                Ok(count) => remaining = &remaining[count..],
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(1))
                }
                Err(_) => return None,
            }
        }
        if !remaining.is_empty() {
            return None;
        }
        let response = self.read(deadline)?;
        (response.get("sequence")?.as_u64()? == self.sequence).then(|| response["answer"].clone())
    }
}
static HELPER: Mutex<(Option<Helper>, Option<Instant>)> = Mutex::new((None, None));

pub fn request(request: Value) -> Option<Value> {
    if !crate::settings::flag("RECAST_DESKTOP_HELPER", false) {
        return None;
    }
    let mut state = HELPER.try_lock().ok()?;
    if state.0.is_none() {
        if state
            .1
            .is_some_and(|last| last.elapsed() < Duration::from_secs(5))
        {
            return None;
        }
        state.1 = Some(Instant::now());
        std::thread::spawn(|| {
            let helper = Helper::start();
            if let Ok(mut state) = HELPER.lock() {
                state.0 = helper;
            }
        });
        return None;
    }
    let timeout = match request["action"].as_str()? {
        "focus" => Duration::from_millis(30),
        "replace" => Duration::from_secs(1),
        _ => Duration::from_millis(250),
    };
    let response = state.0.as_mut()?.request(request, timeout);
    if response.is_none() {
        state.0 = None;
        state.1 = Some(Instant::now());
    }
    response.filter(|value| !value.is_null())
}

pub fn focus() -> Option<super::Focus> {
    let value = request(json!({"action":"focus"}))?;
    Some(super::Focus {
        id: value["id"].as_str()?.into(),
        app: Some(value["app"].as_str()?.into()),
    })
}
