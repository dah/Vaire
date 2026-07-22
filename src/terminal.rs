use std::io::{self, stdout};

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

#[derive(Debug, Default)]
pub struct SystemTerminalOps;

impl TerminalOps for SystemTerminalOps {
    fn enter(&mut self) -> io::Result<()> {
        enter_with_restore(
            enable_raw_mode,
            || execute!(stdout(), EnterAlternateScreen, EnableBracketedPaste, Hide),
            restore_system_terminal,
        )
    }

    fn restore(&mut self) -> io::Result<()> {
        restore_system_terminal()
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

fn restore_system_terminal() -> io::Result<()> {
    let screen_result = execute!(stdout(), Show, DisableBracketedPaste, LeaveAlternateScreen);
    let raw_result = disable_raw_mode();
    screen_result.and(raw_result)
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
