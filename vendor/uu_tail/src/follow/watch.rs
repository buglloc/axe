// This file is part of the uutils coreutils package.
//
// For the full copyright and license information, please view the LICENSE
// file that was distributed with this source code.

use crate::args::{FollowMode, Settings};
use crate::follow::files::{FileHandling, PathData};
use crate::{platform, text};
use notify::{RecommendedWatcher, RecursiveMode, Watcher, WatcherKind};
use std::fs::File;
use std::io::{BufReader, stdout};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, sync_channel};
use std::time::Instant;
use uucore::error::{UResult, USimpleError};
#[cfg(target_os = "linux")]
use uucore::signals::ensure_stdout_not_broken;
use uucore::{show_error, translate};

struct WatcherRx {
    watcher: RecommendedWatcher,
    receiver: Receiver<()>,
}

impl WatcherRx {
    fn watch_file(&mut self, path: &Path) {
        // Both watches are useful: the file catches writes through aliases, and the parent
        // catches pathname replacement. Failure of either is covered by timed reconciliation.
        let _ = self.watcher.watch(path, RecursiveMode::NonRecursive);
        if let Some(parent) = path.parent() {
            let _ = self.watcher.watch(parent, RecursiveMode::NonRecursive);
        }
    }

    fn refresh(&mut self, path: &Path) {
        let _ = self.watcher.unwatch(path);
        self.watch_file(path);
    }
}

pub struct Observer {
    retry: bool,
    follow: Option<FollowMode>,
    pub use_polling: bool,
    watcher_rx: Option<WatcherRx>,
    files: FileHandling,
    pid: platform::Pid,
}

impl Observer {
    pub fn from(settings: &Settings) -> Self {
        let pid = settings.pid.unwrap_or_default();
        Self {
            retry: settings.retry,
            follow: settings.follow,
            use_polling: settings.use_polling,
            watcher_rx: None,
            files: FileHandling::from(settings),
            pid: if platform::supports_pid_checks(pid) {
                pid
            } else {
                0
            },
        }
    }

    pub fn add_path(
        &mut self,
        path: &Path,
        display_name: &str,
        reader: Option<BufReader<File>>,
        update_last: bool,
    ) -> UResult<()> {
        if self.follow.is_some() {
            let stdin = path == Path::new(text::DASH) || path == Path::new(text::DEV_STDIN);
            let path = if path.is_relative() && !stdin {
                std::env::current_dir()?.join(path)
            } else {
                path.to_owned()
            };
            if let Some(watcher) = self.watcher_rx.as_mut()
                && !stdin
            {
                watcher.watch_file(&path);
            }
            self.files.insert(
                PathData::new(path, display_name, reader, stdin)?,
                update_last,
            );
        }
        Ok(())
    }

    pub fn add_bad_path(
        &mut self,
        path: &Path,
        display_name: &str,
        update_last: bool,
    ) -> UResult<()> {
        if self.retry && self.follow.is_some() {
            self.add_path(path, display_name, None, update_last)?;
        }
        Ok(())
    }

    pub fn start(&mut self, _settings: &Settings) -> UResult<()> {
        if self.follow.is_none() {
            return Ok(());
        }
        if self.use_polling || RecommendedWatcher::kind() == WatcherKind::PollWatcher {
            self.use_polling = true;
            return Ok(());
        }
        let (tx, receiver) = sync_channel(1);
        let wake = move |_: notify::Result<notify::Event>| {
            // Events carry no state; one pending wakeup covers any number of changes.
            let _ = tx.try_send(());
        };
        match RecommendedWatcher::new(wake, notify::Config::default()) {
            Ok(watcher) => {
                self.watcher_rx = Some(WatcherRx { watcher, receiver });
            }
            Err(_) => self.fallback(),
        }
        Ok(())
    }

    fn fallback(&mut self) {
        show_error!(
            "{}",
            translate!("tail-status-backend-cannot-be-used-reverting-to-polling", "backend" => text::BACKEND)
        );
        self.watcher_rx = None;
        self.use_polling = true;
    }

    pub fn follow_name_retry(&self) -> bool {
        self.follow == Some(FollowMode::Name) && self.retry
    }

    pub fn has_readers(&self) -> bool {
        self.files.has_readers()
    }
}

pub fn follow(mut observer: Observer, settings: &Settings) -> UResult<()> {
    if !observer.files.has_files() {
        return Err(USimpleError::new(1, translate!("tail-no-files-remaining")));
    }
    let process = platform::ProcessChecker::new(observer.pid);
    let mut last_check = Instant::now();
    let mut writer = stdout().lock();
    loop {
        if observer.pid != 0 && process.is_dead() {
            break;
        }
        let mut more = false;
        for index in 0..observer.files.entries.len() {
            let data = &mut observer.files.entries[index];
            if data.reconcile(settings)
                && let Some(watcher) = observer.watcher_rx.as_mut()
            {
                watcher.refresh(&data.path);
            }
            more |= observer.files.tail_file(index, &mut writer)?;
        }
        if !observer.files.has_files() {
            return Err(USimpleError::new(1, translate!("tail-no-files-remaining")));
        }
        if more {
            continue;
        }
        // Events are only wakeups. The deadline is not extended by events from other
        // files, so aliases, lost watches and mount changes always get a timed scan.
        let elapsed = last_check.elapsed();
        if elapsed >= settings.sleep_sec {
            last_check = Instant::now();
            #[cfg(target_os = "linux")]
            if let Ok(false) = ensure_stdout_not_broken() {
                return Ok(());
            }
        }
        let timeout = settings.sleep_sec.saturating_sub(last_check.elapsed());
        if let Some(watcher) = observer.watcher_rx.as_ref() {
            match watcher.receiver.recv_timeout(timeout) {
                Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => observer.fallback(),
            }
        } else {
            std::thread::sleep(timeout);
        }
    }
    Ok(())
}
