use std::fs::{File, OpenOptions};
use std::io::{self, stdout};
use std::os::fd::AsRawFd;

use crossterm::{
    cursor::{Hide, Show},
    event::{DisableBracketedPaste, EnableBracketedPaste},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};

pub trait TerminalOps {
    fn enter(&mut self) -> io::Result<()>;
    fn restore(&mut self) -> io::Result<()>;
}

pub struct SystemTerminalOps {
    tty: File,
    original_mode: libc::termios,
}

impl SystemTerminalOps {
    pub fn capture() -> io::Result<Self> {
        let tty = OpenOptions::new().read(true).write(true).open("/dev/tty")?;
        let mut original_mode = std::mem::MaybeUninit::<libc::termios>::uninit();
        // SAFETY: `original_mode` points to writable storage for one termios value, the tty fd is
        // valid for the lifetime of this object, and tcgetattr retains neither pointer nor fd.
        if unsafe { libc::tcgetattr(tty.as_raw_fd(), original_mode.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: tcgetattr returned success and therefore initialized the value.
        let original_mode = unsafe { original_mode.assume_init() };
        Ok(Self { tty, original_mode })
    }

    fn restore_original_mode(&self) -> io::Result<()> {
        // SAFETY: both the tty fd and termios reference remain valid for the duration of the call;
        // tcsetattr retains neither.
        if unsafe { libc::tcsetattr(self.tty.as_raw_fd(), libc::TCSANOW, &self.original_mode) } == 0
        {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

impl TerminalOps for SystemTerminalOps {
    fn enter(&mut self) -> io::Result<()> {
        enter_with_restore(
            enable_raw_mode,
            || execute!(stdout(), EnterAlternateScreen, EnableBracketedPaste, Hide),
            || restore_system_terminal(self),
        )
    }

    fn restore(&mut self) -> io::Result<()> {
        restore_system_terminal(self)
    }
}

fn enter_with_restore<Enable, Enter, Restore>(
    enable_raw: Enable,
    enter_screen: Enter,
    mut restore: Restore,
) -> io::Result<()>
where
    Enable: FnOnce() -> io::Result<()>,
    Enter: FnOnce() -> io::Result<()>,
    Restore: FnMut() -> io::Result<()>,
{
    enable_raw()?;
    if let Err(error) = enter_screen() {
        if restore().is_err() {
            // The restore sequence is idempotent. A second attempt covers transient failures in
            // the partial-entry path, where no guard can be returned to retry during Drop.
            let _ = restore();
        }
        return Err(error);
    }
    Ok(())
}

fn restore_system_terminal(terminal: &SystemTerminalOps) -> io::Result<()> {
    let screen_result = execute!(stdout(), Show, DisableBracketedPaste, LeaveAlternateScreen);
    let raw_result = disable_raw_mode();
    let original_result = terminal.restore_original_mode();
    screen_result.and(raw_result).and(original_result)
}

/// Owns terminal mode restoration. Explicit restoration and Drop are idempotent.
pub struct TerminalGuard<T: TerminalOps> {
    ops: T,
    active: bool,
}

impl<T: TerminalOps> TerminalGuard<T> {
    pub fn enter(mut ops: T) -> io::Result<Self> {
        ops.enter()?;
        Ok(Self { ops, active: true })
    }

    pub fn restore(&mut self) -> io::Result<()> {
        if !self.active {
            return Ok(());
        }
        let result = self.ops.restore();
        if result.is_ok() {
            self.active = false;
        }
        result
    }

    /// Temporarily yields terminal I/O to a native foreground CLI.
    ///
    /// This is an alias for restoration so callers cannot accidentally leave raw mode or the
    /// alternate screen active while another process owns stdin/stdout.
    pub fn suspend(&mut self) -> io::Result<()> {
        self.restore()
    }

    /// Forces the terminal back to its normal state after a native foreground CLI used it.
    ///
    /// A child killed by a signal may not restore tty modes before exiting. Marking the guard
    /// active before the idempotent restore also ensures `Drop` retries a transient failure.
    pub fn normalize_after_foreground_child(&mut self) -> io::Result<()> {
        self.active = true;
        self.restore()
    }

    /// Re-enters Vairë's terminal mode after a foreground child has exited.
    pub fn resume(&mut self) -> io::Result<()> {
        if self.active {
            return Ok(());
        }
        self.ops.enter()?;
        self.active = true;
        Ok(())
    }
}

impl<T: TerminalOps> Drop for TerminalGuard<T> {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    use super::{enter_with_restore, TerminalGuard, TerminalOps};

    #[derive(Clone)]
    struct MockOps {
        enters: Arc<AtomicUsize>,
        restores: Arc<AtomicUsize>,
    }

    struct FailOnceRestoreOps {
        restores: Arc<AtomicUsize>,
    }

    impl TerminalOps for MockOps {
        fn enter(&mut self) -> io::Result<()> {
            self.enters.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn restore(&mut self) -> io::Result<()> {
            self.restores.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    impl TerminalOps for FailOnceRestoreOps {
        fn enter(&mut self) -> io::Result<()> {
            Ok(())
        }

        fn restore(&mut self) -> io::Result<()> {
            let attempt = self.restores.fetch_add(1, Ordering::SeqCst);
            if attempt == 0 {
                Err(io::Error::other("first restore failed"))
            } else {
                Ok(())
            }
        }
    }

    fn mock() -> (MockOps, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let enters = Arc::new(AtomicUsize::new(0));
        let restores = Arc::new(AtomicUsize::new(0));
        (
            MockOps {
                enters: Arc::clone(&enters),
                restores: Arc::clone(&restores),
            },
            enters,
            restores,
        )
    }

    #[test]
    fn explicit_and_drop_restoration_are_idempotent() {
        let (ops, enters, restores) = mock();
        let mut guard = TerminalGuard::enter(ops).unwrap();
        guard.restore().unwrap();
        guard.restore().unwrap();
        drop(guard);
        assert_eq!(enters.load(Ordering::SeqCst), 1);
        assert_eq!(restores.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn unwinding_restores_once() {
        let (ops, _, restores) = mock();
        let result = catch_unwind(AssertUnwindSafe(|| {
            let _guard = TerminalGuard::enter(ops).unwrap();
            panic!("test unwind");
        }));
        assert!(result.is_err());
        assert_eq!(restores.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn failed_explicit_restore_is_retried_on_drop() {
        let restores = Arc::new(AtomicUsize::new(0));
        let mut guard = TerminalGuard::enter(FailOnceRestoreOps {
            restores: Arc::clone(&restores),
        })
        .unwrap();

        assert!(guard.restore().is_err());
        drop(guard);

        assert_eq!(restores.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn suspend_and_resume_transfer_terminal_ownership_in_order() {
        let (ops, enters, restores) = mock();
        let mut guard = TerminalGuard::enter(ops).unwrap();

        guard.suspend().unwrap();
        guard.suspend().unwrap();
        assert_eq!(enters.load(Ordering::SeqCst), 1);
        assert_eq!(restores.load(Ordering::SeqCst), 1);

        guard.resume().unwrap();
        guard.resume().unwrap();
        assert_eq!(enters.load(Ordering::SeqCst), 2);
        assert_eq!(restores.load(Ordering::SeqCst), 1);

        drop(guard);
        assert_eq!(restores.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn foreground_child_normalization_restores_even_while_suspended() {
        let (ops, enters, restores) = mock();
        let mut guard = TerminalGuard::enter(ops).unwrap();
        guard.suspend().unwrap();

        guard.normalize_after_foreground_child().unwrap();

        assert_eq!(enters.load(Ordering::SeqCst), 1);
        assert_eq!(restores.load(Ordering::SeqCst), 2);
        drop(guard);
        assert_eq!(restores.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn failed_suspend_never_allows_a_second_enter() {
        let restores = Arc::new(AtomicUsize::new(0));
        let mut guard = TerminalGuard::enter(FailOnceRestoreOps {
            restores: Arc::clone(&restores),
        })
        .unwrap();

        assert!(guard.suspend().is_err());
        // The guard still considers itself active after a failed restore, so resume is a no-op.
        guard.resume().unwrap();
        drop(guard);

        assert_eq!(restores.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn partial_screen_entry_retries_a_transient_restore_failure() {
        let restores = Arc::new(AtomicUsize::new(0));
        let restore_count = Arc::clone(&restores);
        let error = enter_with_restore(
            || Ok(()),
            || Err(io::Error::other("partial screen entry")),
            move || {
                let attempt = restore_count.fetch_add(1, Ordering::SeqCst);
                if attempt == 0 {
                    Err(io::Error::other("transient restore failure"))
                } else {
                    Ok(())
                }
            },
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert_eq!(restores.load(Ordering::SeqCst), 2);
    }
}
