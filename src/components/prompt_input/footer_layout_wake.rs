//! PromptInput-scoped footer layout wake.
//!
//! L1: `PromptInput-scoped footer layout wake` (PORTING.md). CC has no such
//! carrier — Ink auto-layout means each footer row's local `setState`
//! re-renders that child and the terminal reflows (Notifications.tsx:237-250
//! apiKeyHelper 1s poll; :362-363 self-contained `<SandboxPromptFooterHint />`
//! child). Nyan PromptInput budgets its
//! own height retained-style, so a row visibility flip must wake PromptInput
//! — and only PromptInput — to recompute heights from the live sources.
//!
//! Ownership: PromptInput creates the wake via [`use_footer_layout_wake`] and
//! injects it through a narrow `ContextProvider` around its Footer subtree
//! (Footer / Notifications / SandboxPromptFooterHint).
//! Both producers are event-driven, never periodic: the apiKeyHelper 1s poll
//! bumps ONLY on visibility flips; the sandbox
//! violation subscription + 5s decay bumps on recent-count changes (post
//! S9-1 its render text derives from the same module atomic the height
//! helpers read, so count changes must re-render; visibility flips are a
//! subset). Both height-compute sites (prompt_input/mod.rs and
//! prompt_input_footer.rs `Footer`) read the same epoch. This replaces the
//! deleted `AppState.footer_layout_epoch`, whose bump woke the whole tree via
//! the AppStore version counter on every 1s row flip (performance bug);
//! layout/text wakes no longer invoke AppStore `on_change` or listeners.
//!
//! CAUTION ruling (recorded 2026-08-01, this batch): producers bump from
//! tokio threads. A `State<u64>` carrier would rely on cross-thread
//! `State::set` → `try_write`, which silently drops the bump under borrow
//! contention — the class Contract C ("iocraft coherent AppState frame
//! carrier", PORTING.md) flags and resolves with an atomic wake hook. This
//! carrier follows Contract C's wake discipline: an `AtomicU64` epoch
//! (`fetch_add` — lossless, no drop path) plus a register-then-recheck waker
//! [`Hook`] owned by PromptInput. No bounded-staleness argument is needed
//! because no bump can be lost. After PromptInput unmounts, bumps still
//! advance the epoch but wake at most one stale waker (a safe no-op re-poll);
//! the slot is only refilled by `poll_change`, so wakes stop with the owner.
//! The same discipline extends to producer payloads (review A-S9-1): the
//! sandbox recent count lives in a module atomic read by both height and
//! render (sandbox_prompt_footer_hint.rs), so no footer-wake path performs a
//! cross-thread `State::set`.

use iocraft::prelude::{Hook, Hooks};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll, Waker};

#[derive(Default)]
struct WakeShared {
    epoch: AtomicU64,
    waker: Mutex<Option<Waker>>,
}

/// Cheap-to-clone producer/reader handle. `Send + Sync`, so the tokio-side
/// producers (sandbox subscription, apiKeyHelper poll) can bump
/// it directly — the bump is a lossless atomic increment plus a waker wake.
#[derive(Clone)]
pub struct FooterLayoutWake {
    shared: Arc<WakeShared>,
}

impl FooterLayoutWake {
    /// Detached construction for tests and harnesses: the epoch works, but no
    /// component is woken (no hook polls the shared waker slot). Production
    /// wakes must come from [`use_footer_layout_wake`].
    pub fn detached() -> Self {
        Self {
            shared: Arc::new(WakeShared::default()),
        }
    }

    /// Bump the layout epoch and wake the owning component. Lossless from any
    /// thread (`fetch_add` cannot fail); producers call this on visibility
    /// flips only.
    pub fn bump(&self) {
        self.shared.epoch.fetch_add(1, Ordering::SeqCst);
        let waker = self
            .shared
            .waker
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    /// Current epoch. Height-compute sites read this so the dependency on the
    /// wake carrier is explicit at both sites (re-render itself is driven by
    /// the PromptInput-owned hook).
    pub fn epoch(&self) -> u64 {
        self.shared.epoch.load(Ordering::SeqCst)
    }
}

/// The consumer-side hook: owned by PromptInput, wakes it whenever the epoch
/// advances past the last value seen by `poll_change`.
struct FooterLayoutWakeHook {
    shared: Arc<WakeShared>,
    seen: u64,
}

impl Hook for FooterLayoutWakeHook {
    fn poll_change(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let this = self.get_mut();
        // Register-then-recheck (Contract C): store the waker BEFORE loading
        // the epoch so a producer bump racing this poll either lands in the
        // load below or finds the freshly registered waker — a wake can never
        // fall between the two.
        *this
            .shared
            .waker
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(cx.waker().clone());
        let current = this.shared.epoch.load(Ordering::SeqCst);
        if current != this.seen {
            this.seen = current;
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

/// Create (once) the PromptInput-scoped wake and bind its hook to the calling
/// component. Multiple bumps between polls coalesce into a single re-render
/// (the epoch comparison, mirroring `State`'s `did_change` batching).
pub fn use_footer_layout_wake(hooks: &mut Hooks) -> FooterLayoutWake {
    let hook = hooks.use_hook(|| FooterLayoutWakeHook {
        shared: Arc::new(WakeShared::default()),
        seen: 0,
    });
    FooterLayoutWake {
        shared: Arc::clone(&hook.shared),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::task::Wake;

    struct CountingWaker(AtomicUsize);

    impl Wake for CountingWaker {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn poll(hook: &mut FooterLayoutWakeHook, waker: &Waker) -> Poll<()> {
        Pin::new(hook).poll_change(&mut Context::from_waker(waker))
    }

    #[test]
    fn bump_advances_epoch_and_wakes_registered_poller_losslessly() {
        let wake = FooterLayoutWake::detached();
        let mut hook = FooterLayoutWakeHook {
            shared: Arc::clone(&wake.shared),
            seen: 0,
        };
        let counting = Arc::new(CountingWaker(AtomicUsize::new(0)));
        let waker = Waker::from(Arc::clone(&counting));

        // No flip yet: pending, waker registered.
        assert_eq!(poll(&mut hook, &waker), Poll::Pending);
        assert_eq!(wake.epoch(), 0);

        // Visibility flip: epoch advances, registered waker fires exactly once.
        wake.bump();
        assert_eq!(wake.epoch(), 1);
        assert_eq!(counting.0.load(Ordering::SeqCst), 1);
        assert_eq!(poll(&mut hook, &waker), Poll::Ready(()));
        // Same epoch (visible→visible sample, no bump): no re-render signal.
        assert_eq!(poll(&mut hook, &waker), Poll::Pending);

        // Two flips before the next poll coalesce into one Ready pass.
        wake.bump();
        wake.bump();
        assert_eq!(wake.epoch(), 3);
        assert_eq!(poll(&mut hook, &waker), Poll::Ready(()));
        assert_eq!(poll(&mut hook, &waker), Poll::Pending);
    }

    #[test]
    fn unmount_stops_wakes_and_bumps_stay_safe() {
        let wake = FooterLayoutWake::detached();
        let counting = Arc::new(CountingWaker(AtomicUsize::new(0)));
        let waker = Waker::from(Arc::clone(&counting));
        {
            let mut hook = FooterLayoutWakeHook {
                shared: Arc::clone(&wake.shared),
                seen: 0,
            };
            assert_eq!(poll(&mut hook, &waker), Poll::Pending);
            // Hook (the owning component) drops here — unmount.
        }
        // First post-unmount bump consumes the stale waker (safe no-op class);
        // nothing refills the slot without poll_change, so wakes stop.
        wake.bump();
        assert_eq!(counting.0.load(Ordering::SeqCst), 1);
        wake.bump();
        wake.bump();
        assert_eq!(counting.0.load(Ordering::SeqCst), 1);
        assert_eq!(wake.epoch(), 3);
    }
}
