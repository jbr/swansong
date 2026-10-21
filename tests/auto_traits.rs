//! The public types' auto traits are part of the API. These assertions fail to compile if one
//! changes. The wrappers' auto traits should be exactly those of the wrapped type.

use std::{
    cell::Cell,
    marker::PhantomPinned,
    panic::{RefUnwindSafe, UnwindSafe},
    rc::Rc,
    sync::MutexGuard,
};
use swansong::{
    Guard, GuardReport, GuardReportEntry, Guarded, Interrupt, ShutdownCompletion, ShutdownState,
    ShuttingDown, Swansong,
};

macro_rules! assert_impl {
    ($type:ty: $($trait:path),+) => {
        const _: fn() = || {
            fn assert<T: ?Sized $(+ $trait)+>() {}
            assert::<$type>();
        };
    };
}

/// Compiles only if `$type` does not implement `$trait`: if it does, `some_item` is ambiguous.
macro_rules! assert_not_impl {
    ($type:ty: $trait:path) => {
        const _: fn() = || {
            trait AmbiguousIfImpl<A> {
                fn some_item() {}
            }
            impl<T: ?Sized> AmbiguousIfImpl<()> for T {}
            struct Invalid;
            impl<T: ?Sized + $trait> AmbiguousIfImpl<Invalid> for T {}
            let _ = <$type as AmbiguousIfImpl<_>>::some_item;
        };
    };
}

macro_rules! assert_all_auto_traits {
    ($($type:ty),+ $(,)?) => {
        $(assert_impl!($type: Send, Sync, Unpin, UnwindSafe, RefUnwindSafe);)+
    };
}

assert_all_auto_traits!(
    Swansong,
    Guard,
    ShutdownCompletion,
    ShuttingDown,
    GuardReport,
    GuardReportEntry,
    ShutdownState,
);

macro_rules! assert_wrapper_auto_traits {
    ($wrapper:ident) => {
        assert_all_auto_traits!($wrapper<()>);

        assert_impl!($wrapper<Rc<()>>: Unpin, UnwindSafe, RefUnwindSafe);
        assert_not_impl!($wrapper<Rc<()>>: Send);
        assert_not_impl!($wrapper<Rc<()>>: Sync);

        assert_impl!($wrapper<MutexGuard<'static, ()>>: Sync, Unpin, UnwindSafe, RefUnwindSafe);
        assert_not_impl!($wrapper<MutexGuard<'static, ()>>: Send);

        assert_impl!($wrapper<Cell<()>>: Send, Unpin, UnwindSafe);
        assert_not_impl!($wrapper<Cell<()>>: Sync);
        assert_not_impl!($wrapper<Cell<()>>: RefUnwindSafe);

        assert_impl!($wrapper<PhantomPinned>: Send, Sync, UnwindSafe, RefUnwindSafe);
        assert_not_impl!($wrapper<PhantomPinned>: Unpin);

        assert_impl!($wrapper<&'static mut ()>: Send, Sync, Unpin, RefUnwindSafe);
        assert_not_impl!($wrapper<&'static mut ()>: UnwindSafe);
    };
}

assert_wrapper_auto_traits!(Interrupt);
assert_wrapper_auto_traits!(Guarded);
