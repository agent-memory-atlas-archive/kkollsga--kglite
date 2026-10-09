//! `KgliteCursor`: a read query pulled a batch of rows at a time.
//!
//! [`kglite_session_cursor_open`] returns before any row is built when the query
//! is a plain `MATCH … RETURN <expressions>`; each [`kglite_cursor_next_batch`]
//! then produces the next rows, so a caller that processes and drops each batch
//! holds a few batches, not the result. Queries that need their whole input
//! first (ORDER BY, DISTINCT, aggregation, UNION), and calls that set a row cap,
//! are built whole by the engine as [`kglite_session_execute_read`] builds
//! them; [`kglite_cursor_streamed`] says which. The cursor holds the graph
//! snapshot it was opened on until [`kglite_cursor_free`].

use crate::session::{
    parse_params_json, read_execute_options, report_query_param_error, KgliteExecuteOptions,
    KgliteSession, SessionState,
};
use crate::status::KgliteStatusCode;
use crate::strings::alloc_c_string;
use kglite::api::session::Cursor;
use std::ffi::{c_char, CStr};
use std::sync::{Mutex, PoisonError};

/// Opaque handle for an open read cursor. Free with [`kglite_cursor_free`].
#[repr(C)]
pub struct KgliteCursor {
    _opaque: [u8; 0],
    _marker: core::marker::PhantomData<(*mut u8, core::marker::PhantomPinned)>,
}

struct CursorState {
    // The engine cursor is `Send` but not `Sync`; the lock makes a handle shared
    // across threads safe, serialising `next_batch` calls.
    cursor: Mutex<Cursor>,
    columns: Vec<String>,
    streamed: bool,
    tagged_results: bool,
}

impl CursorState {
    unsafe fn from_handle<'a>(handle: *const KgliteCursor) -> &'a CursorState {
        unsafe { &*handle.cast::<CursorState>() }
    }
}

/// Open a cursor over a read-only Cypher query. Arguments as for
/// [`kglite_session_execute_read_ex`](crate::kglite_session_execute_read_ex):
/// `params_json` null or a JSON object, `options` null or a
/// [`KgliteExecuteOptions`] block. The timeout and cancel token in `options`
/// apply for the cursor's whole life, including between batches; a `row_limit`
/// makes the engine build the result whole (see [`kglite_cursor_streamed`]).
///
/// A parse or planning error is returned here; an execution error arrives from
/// [`kglite_cursor_next_batch`].
///
/// # Safety
///
/// `session` a live session handle; `query` a NUL-terminated string;
/// `out_cursor` a writable `*mut KgliteCursor` slot, set to null on failure and
/// otherwise owned by the caller (free with [`kglite_cursor_free`]). The cursor
/// keeps its own snapshot, so the session may be used or freed afterwards.
#[no_mangle]
pub unsafe extern "C" fn kglite_session_cursor_open(
    session: *const KgliteSession,
    query: *const c_char,
    params_json: *const c_char,
    options: *const KgliteExecuteOptions,
    out_cursor: *mut *mut KgliteCursor,
    out_error_msg: *mut *const c_char,
) -> KgliteStatusCode {
    crate::ffi::status_boundary(
        out_error_msg,
        || crate::ffi::init_out(out_cursor, std::ptr::null_mut()),
        || {
            if session.is_null() || query.is_null() || out_cursor.is_null() {
                return KgliteStatusCode::NullPointer;
            }
            let query_str = match unsafe { CStr::from_ptr(query) }.to_str() {
                Ok(s) => s,
                Err(_) => return KgliteStatusCode::InvalidUtf8,
            };
            let params = match parse_params_json(params_json) {
                Ok(p) => p,
                Err(error) => return report_query_param_error(error, out_error_msg),
            };
            let limits = match read_execute_options(options) {
                Ok(limits) => limits,
                Err(message) => {
                    crate::ffi::init_out(out_error_msg, alloc_c_string(&message));
                    return KgliteStatusCode::InvalidArgument;
                }
            };
            let state = unsafe { SessionState::from_handle(session) };
            let mut opts = state.make_opts(&params);
            limits.apply(&mut opts);
            match state.inner.execute_read_cursor(query_str, &opts) {
                Ok(cursor) => {
                    let handle = CursorState {
                        columns: cursor.columns().to_vec(),
                        streamed: cursor.streamed(),
                        tagged_results: state.tagged_results(),
                        cursor: Mutex::new(cursor),
                    };
                    unsafe { *out_cursor = Box::into_raw(Box::new(handle)).cast::<KgliteCursor>() };
                    KgliteStatusCode::Ok
                }
                Err(err) => {
                    crate::ffi::init_out(out_error_msg, alloc_c_string(&err.to_string()));
                    KgliteStatusCode::from_kg_error(&err)
                }
            }
        },
    )
}

/// The result column names as an owned JSON array string, known before the
/// first row. Free with [`kglite_free_string`](crate::kglite_free_string).
/// Null for a null handle.
///
/// # Safety
///
/// `cursor` null or a live cursor handle.
#[no_mangle]
pub unsafe extern "C" fn kglite_cursor_columns_json(cursor: *const KgliteCursor) -> *const c_char {
    crate::ffi::value_boundary(std::ptr::null(), || {
        if cursor.is_null() {
            return std::ptr::null();
        }
        let state = unsafe { CursorState::from_handle(cursor) };
        match serde_json::to_string(&state.columns) {
            Ok(s) => alloc_c_string(&s),
            Err(_) => std::ptr::null(),
        }
    })
}

/// Whether rows are produced as they are pulled (`true`) or the engine built
/// the whole result before the first batch (`false`), in which case the cursor
/// only slices it. A null handle returns `false`.
///
/// # Safety
///
/// `cursor` null or a live cursor handle.
#[no_mangle]
pub unsafe extern "C" fn kglite_cursor_streamed(cursor: *const KgliteCursor) -> bool {
    crate::ffi::value_boundary(false, || {
        !cursor.is_null() && unsafe { CursorState::from_handle(cursor) }.streamed
    })
}

/// Pull up to `max_rows` further rows (at least one is asked for) as an owned
/// JSON array of row objects keyed by column name, in the encoding of
/// [`kglite_cypher_result_rows_json`](crate::kglite_cypher_result_rows_json).
/// An empty array (`[]`) with `KGLITE_STATUS_CODE_OK` means the cursor is
/// exhausted. A failure, cancellation or timeout while producing rows returns
/// its status here, once, and ends the cursor: later calls return `[]`.
///
/// Free `*out_rows_json` with [`kglite_free_string`](crate::kglite_free_string).
///
/// # Safety
///
/// `cursor` a live handle; `out_rows_json` a writable slot, set to null on
/// failure.
#[no_mangle]
pub unsafe extern "C" fn kglite_cursor_next_batch(
    cursor: *mut KgliteCursor,
    max_rows: usize,
    out_rows_json: *mut *const c_char,
    out_error_msg: *mut *const c_char,
) -> KgliteStatusCode {
    crate::ffi::status_boundary(
        out_error_msg,
        || crate::ffi::init_out(out_rows_json, std::ptr::null()),
        || {
            if cursor.is_null() || out_rows_json.is_null() {
                return KgliteStatusCode::NullPointer;
            }
            let state = unsafe { CursorState::from_handle(cursor) };
            let batch = state
                .cursor
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .next_batch(max_rows);
            match batch {
                Ok(rows) => {
                    let json =
                        crate::result::rows_to_json(&state.columns, &rows, state.tagged_results);
                    match serde_json::to_string(&json) {
                        Ok(text) => {
                            unsafe { *out_rows_json = alloc_c_string(&text) };
                            KgliteStatusCode::Ok
                        }
                        Err(e) => {
                            crate::ffi::init_out(out_error_msg, alloc_c_string(&e.to_string()));
                            KgliteStatusCode::Internal
                        }
                    }
                }
                Err(err) => {
                    crate::ffi::init_out(out_error_msg, alloc_c_string(&err.to_string()));
                    KgliteStatusCode::from_kg_error(&err)
                }
            }
        },
    )
}

/// Free a cursor handle, stopping its engine worker and releasing the snapshot
/// it held. Idempotent on null.
///
/// # Safety
///
/// `cursor` null or a handle from [`kglite_session_cursor_open`] not yet freed,
/// and not in use by another call.
#[no_mangle]
pub unsafe extern "C" fn kglite_cursor_free(cursor: *mut KgliteCursor) {
    crate::ffi::void_boundary(|| {
        if !cursor.is_null() {
            let _ = unsafe { Box::from_raw(cursor.cast::<CursorState>()) };
        }
    });
}
