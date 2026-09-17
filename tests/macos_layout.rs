// Exercise the actual Carbon wrapper from a worker, without changing layouts.
// A normal Rust test harness runs tests off the main thread without a run loop.
#[cfg(target_os = "macos")]
mod types {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Language {
        English,
        Hebrew,
    }
}

#[cfg(target_os = "macos")]
#[derive(Debug, PartialEq, Eq)]
enum LayoutSwitch {
    AlreadyThere,
    Switched,
    Failed,
}

#[cfg(target_os = "macos")]
fn set_layout_cache(_: types::Language) {}

#[cfg(target_os = "macos")]
fn current_layout() -> Option<types::Language> {
    macos::query_layout()
}

#[cfg(target_os = "macos")]
#[path = "../src/layout/macos.rs"]
mod macos;

#[cfg(target_os = "macos")]
fn main() {
    use core_foundation_sys::runloop::{kCFRunLoopDefaultMode, CFRunLoopRunInMode};
    use std::time::{Duration, Instant};

    let expected = macos::enabled_languages();
    let worker = std::thread::spawn(move || {
        assert_eq!(macos::enabled_languages(), expected);
        if let Some(lang) = macos::query_layout() {
            assert_eq!(macos::switch_layout_to(lang), LayoutSwitch::AlreadyThere);
        }
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while !worker.is_finished() {
        assert!(
            Instant::now() < deadline,
            "main-queue layout query deadlocked"
        );
        unsafe {
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.01, 1);
        }
    }
    worker.join().unwrap();
    println!("macOS input-source queries passed on main and worker threads");
}

#[cfg(not(target_os = "macos"))]
fn main() {}
