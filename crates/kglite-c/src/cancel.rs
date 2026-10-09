//! `KgliteCancelToken`: a handle that stops a running query from another
//! thread. Create one, attach it to a call through
//! [`KgliteExecuteOptions::cancel`](crate::KgliteExecuteOptions), and call
//! [`kglite_cancel_token_cancel`] from anywhere.

use crate::status::KgliteStatusCode;
use kglite::api::session::CancelToken;

/// Opaque handle for a cancellation token. See
/// [`KgliteGraph`](crate::KgliteGraph) for the empty-`#[repr(C)]` facade.
#[repr(C)]
pub struct KgliteCancelToken {
    _opaque: [u8; 0],
    _marker: core::marker::PhantomData<(*mut u8, core::marker::PhantomPinned)>,
}

impl KgliteCancelToken {
    /// A reference-counted copy that outlives this handle.
    pub(crate) fn clone_token(&self) -> CancelToken {
        let token: &CancelToken = unsafe { &*std::ptr::from_ref(self).cast::<CancelToken>() };
        token.clone()
    }
}

/// Create a cancellation token.
///
/// Attach it to a query through `KgliteExecuteOptions.cancel` on
/// `kglite_session_execute_read_ex`, `kglite_session_execute_mut_ex` or
/// `kglite_tx_execute`. A token that has been cancelled stays cancelled: a
/// later call that carries it returns `KGLITE_STATUS_CODE_CANCELLED` at once.
/// Make one token per query you may want to stop.
///
/// # Arguments
///
/// - `out_token` (out, owned): the new token; free it with
///   [`kglite_cancel_token_free`].
///
/// # Safety
///
/// `out_token` a valid writable slot.
#[no_mangle]
pub unsafe extern "C" fn kglite_cancel_token_new(
    out_token: *mut *mut KgliteCancelToken,
) -> KgliteStatusCode {
    crate::ffi::status_boundary(
        std::ptr::null_mut(),
        || crate::ffi::init_out(out_token, std::ptr::null_mut()),
        || {
            if out_token.is_null() {
                return KgliteStatusCode::NullPointer;
            }
            let boxed = Box::new(CancelToken::new());
            unsafe { *out_token = Box::into_raw(boxed).cast::<KgliteCancelToken>() };
            KgliteStatusCode::Ok
        },
    )
}

/// Ask every call carrying `token` to stop. Safe to call from any thread, at
/// any time, any number of times; it only sets a flag. A running query
/// returns `KGLITE_STATUS_CODE_CANCELLED` at its next check, without
/// publishing a partial write. A query that has already finished is
/// unaffected.
///
/// # Safety
///
/// `token` a handle from [`kglite_cancel_token_new`] not yet freed. Do not
/// call this concurrently with [`kglite_cancel_token_free`] on the same
/// handle.
#[no_mangle]
pub unsafe extern "C" fn kglite_cancel_token_cancel(
    token: *const KgliteCancelToken,
) -> KgliteStatusCode {
    crate::ffi::status_boundary(
        std::ptr::null_mut(),
        || {},
        || {
            if token.is_null() {
                return KgliteStatusCode::NullPointer;
            }
            unsafe { &*token }.clone_token().cancel();
            KgliteStatusCode::Ok
        },
    )
}

/// Free a token. Null is a no-op. A call already running with the token keeps
/// its own reference and is unaffected; do not pass the handle to a new call
/// or to [`kglite_cancel_token_cancel`] afterwards.
///
/// # Safety
///
/// `token` null or a handle from [`kglite_cancel_token_new`] not yet freed.
#[no_mangle]
pub unsafe extern "C" fn kglite_cancel_token_free(token: *mut KgliteCancelToken) {
    crate::ffi::void_boundary(|| {
        if !token.is_null() {
            drop(unsafe { Box::from_raw(token.cast::<CancelToken>()) });
        }
    });
}
