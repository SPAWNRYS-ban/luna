use std::process::Child;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

/// Cleanup also runs on a child wait error or an unwinding package task.
pub(crate) struct ManagedGroup {
    child: Child,
    quiet: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
    cleaned: bool,
}
impl ManagedGroup {
    pub(crate) fn new(child: Child, quiet: Arc<AtomicBool>, failed: Arc<AtomicBool>) -> Self {
        failed.store(true, Ordering::SeqCst);
        Self {
            child,
            quiet,
            failed,
            cleaned: false,
        }
    }
    pub(crate) fn stop(&mut self) {
        if self.cleaned {
            return;
        }
        let quiet = stop_process_group(&mut self.child);
        self.quiet.store(quiet, Ordering::SeqCst);
        self.failed.store(!quiet, Ordering::SeqCst);
        self.cleaned = true;
    }
}
impl std::ops::Deref for ManagedGroup {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.child
    }
}
impl std::ops::DerefMut for ManagedGroup {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.child
    }
}
impl Drop for ManagedGroup {
    fn drop(&mut self) {
        self.stop();
    }
}

pub(crate) fn stop_process_group(child: &mut Child) -> bool {
    // Leave room for the audio guard's 10-second recovery wait and supervisor cleanup.
    stop_with_grace(child, Duration::from_secs(15), Duration::from_secs(2));
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        if group_is_quiet(child.id()) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(target_os = "linux")]
fn group_is_quiet(group: u32) -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return false;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().parse::<u32>().is_err() {
            continue;
        }
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        let Some((_, fields)) = stat.rsplit_once(") ") else {
            continue;
        };
        let fields: Vec<_> = fields.split_whitespace().collect();
        if fields.get(2).and_then(|v| v.parse::<u32>().ok()) == Some(group)
            && !matches!(fields.first(), Some(&"Z" | &"X"))
        {
            return false;
        }
    }
    true
}
#[cfg(all(unix, not(target_os = "linux")))]
fn group_is_quiet(group: u32) -> bool {
    (unsafe { libc::kill(-(group as libc::pid_t), 0) }) == -1
        && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}
#[cfg(not(unix))]
fn group_is_quiet(_: u32) -> bool {
    false
}

/// Give the package supervisor time to restore shared resources while its
/// dependencies are still alive, then remove any remaining group members.
/// The caller must have spawned the child with `process_group(0)`.
fn stop_with_grace(child: &mut Child, grace: Duration, fallback: Duration) {
    #[cfg(unix)]
    {
        let pid = child.id() as libc::pid_t;
        let group = -pid;
        if matches!(child.try_wait(), Ok(None)) {
            unsafe { libc::kill(pid, libc::SIGTERM) };
            let deadline = std::time::Instant::now() + grace;
            while std::time::Instant::now() < deadline {
                if !matches!(child.try_wait(), Ok(None)) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }

        // The supervisor may already have exited while leaving helpers alive.
        unsafe { libc::kill(group, libc::SIGTERM) };
        let deadline = std::time::Instant::now() + fallback;
        while std::time::Instant::now() < deadline {
            let _ = child.try_wait();
            if unsafe { libc::kill(group, 0) } == -1
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        unsafe { libc::kill(group, libc::SIGKILL) };
    }
    #[cfg(not(unix))]
    let _ = (grace, fallback);
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::process::CommandExt;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    struct Fixture {
        child: Child,
        dir: PathBuf,
    }

    impl Fixture {
        fn start(script: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "luna-stop-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&dir).unwrap();
            let child = Command::new("sh")
                .args(["-c", script])
                .current_dir(&dir)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .process_group(0)
                .spawn()
                .unwrap();
            let fixture = Self { child, dir };
            fixture.wait_for("ready");
            fixture
        }

        fn wait_for(&self, file: &str) {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !self.dir.join(file).exists() {
                assert!(Instant::now() < deadline, "fixture did not create {file}");
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        fn stop(&mut self) {
            stop_with_grace(
                &mut self.child,
                Duration::from_secs(1),
                Duration::from_millis(200),
            );
        }

        fn assert_helper_stopped(&self) {
            let pid = fs::read_to_string(self.dir.join("helper.pid")).unwrap();
            let stat = format!("/proc/{}/stat", pid.trim());
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                match fs::read_to_string(&stat) {
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
                    Ok(s) if s.rsplit_once(") ").unwrap().1.starts_with('Z') => return,
                    _ => {}
                }
                assert!(Instant::now() < deadline, "helper remains running");
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            unsafe {
                libc::kill(-(self.child.id() as libc::pid_t), libc::SIGKILL);
            }
            let _ = self.child.wait();
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn supervisor_can_recover_before_dependencies_receive_term() {
        let mut f = Fixture::start(
            r#"
            sh -c 'trap "touch helper-stopped; exit 0" TERM; touch helper-ready; while :; do sleep 0.02; done' &
            helper=$!
            echo "$helper" > helper.pid
            while [ ! -f helper-ready ]; do sleep 0.01; done
            trap 'sleep 0.15; if [ ! -f helper-stopped ]; then touch recovered; fi; kill "$helper"; wait "$helper"; exit 0' TERM
            touch ready
            while :; do sleep 0.02; done
        "#,
        );
        stop_process_group(&mut f.child);
        assert!(
            f.dir.join("recovered").exists(),
            "dependency terminated before recovery completed"
        );
        f.assert_helper_stopped();
    }

    #[test]
    fn stubborn_supervisor_and_helper_are_killed_within_bound() {
        let mut f = Fixture::start(
            r#"
            trap '' TERM
            sh -c 'trap "" TERM; touch helper-ready; while :; do sleep 0.02; done' &
            echo "$!" > helper.pid
            while [ ! -f helper-ready ]; do sleep 0.01; done
            touch ready
            while :; do sleep 0.02; done
        "#,
        );
        let started = Instant::now();
        f.stop();
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(f.child.try_wait().unwrap().is_some());
        f.assert_helper_stopped();
    }

    #[test]
    fn exited_supervisor_does_not_leave_stubborn_helper() {
        let mut f = Fixture::start(
            r#"
            sh -c 'trap "" TERM; touch helper-ready; while :; do sleep 0.02; done' &
            echo "$!" > helper.pid
            while [ ! -f helper-ready ]; do sleep 0.01; done
            touch ready
            exit 0
        "#,
        );
        f.child.wait().unwrap();
        f.stop();
        f.assert_helper_stopped();
    }
}
