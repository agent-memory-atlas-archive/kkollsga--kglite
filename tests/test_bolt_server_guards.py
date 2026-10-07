"""Protocol-level guards of kglite-bolt-server, exercised with a raw Bolt client.

`boltr` 0.2.0 lets a client that failed to authenticate reach `Ready` through
RESET, accepts messages of any size and nesting before authentication, and
leaks a session when a client vanishes mid-response. The accept loop and the
backend wrapper in `crates/kglite-bolt-server/src/guard.rs` close those gaps;
these tests drive them with hand-built messages no official driver would send.
"""

import socket
import struct
import time

import pytest

from tests.bolt_raw import (
    SIG_BEGIN,
    SIG_COMMIT,
    SIG_FAILURE,
    SIG_HELLO,
    SIG_PULL,
    SIG_RECORD,
    SIG_RESET,
    SIG_RUN,
    SIG_SUCCESS,
    RawBolt,
    nested_lists,
    pack,
)

pytestmark = [pytest.mark.bolt]

USER, PASSWORD = "alice", "secret"


def _hostport(url: str) -> tuple[str, int]:
    host, port = url.removeprefix("bolt://").rsplit(":", 1)
    return host, int(port)


@pytest.fixture
def auth_server(tmp_path, bolt_binary_path):
    if not bolt_binary_path.exists():
        pytest.skip("bolt-server binary not built")
    from tests.conftest import _build_bolt_fixture_graph, _spawn_bolt_server, _teardown_bolt_server

    graph = tmp_path / "auth.kgl"
    _build_bolt_fixture_graph(graph)
    proc, url = _spawn_bolt_server(graph, extra_args=["--auth", "basic", "--auth-user", USER, "--auth-pass", PASSWORD])
    yield _hostport(url)
    _teardown_bolt_server(proc)


def _counts_after_reset_ok(conn: RawBolt) -> bool:
    """RESET, then try to run a query. True when the query ran (a bypass)."""
    try:
        conn.request(SIG_RESET)
        sig = conn.request(SIG_RUN, "MATCH (n:Person) RETURN count(n) AS c", {}, {})
    except (EOFError, ConnectionError):
        return False
    return sig == SIG_SUCCESS


# --- authentication bypass ---------------------------------------------------


def test_wrong_password_then_reset_cannot_run_queries(auth_server):
    with RawBolt(*auth_server) as conn:
        assert conn.hello() == SIG_SUCCESS
        assert conn.logon(USER, "wrong") == SIG_FAILURE
        assert not _counts_after_reset_ok(conn), "RESET after a failed LOGON reached Ready and ran a query"


def test_wrong_password_closes_the_connection(auth_server):
    with RawBolt(*auth_server) as conn:
        conn.hello()
        assert conn.logon(USER, "wrong") == SIG_FAILURE
        assert conn.is_closed()


def test_wrong_password_then_reset_cannot_write(auth_server):
    with RawBolt(*auth_server) as conn:
        conn.hello()
        conn.logon(USER, "wrong")
        committed = False
        try:
            conn.request(SIG_RESET)
            if conn.request(SIG_BEGIN, {}) == SIG_SUCCESS:
                conn.request(SIG_RUN, "CREATE (:Person {id: 99, title: 'Intruder'})", {}, {})
                conn.request(SIG_PULL, {"n": -1})
                committed = conn.request(SIG_COMMIT) == SIG_SUCCESS
        except (EOFError, ConnectionError):
            pass
        assert not committed, "an unauthenticated client committed a write"
    # A correctly authenticated client still sees only the four fixture nodes.
    with RawBolt(*auth_server) as conn:
        conn.hello()
        assert conn.logon(USER, PASSWORD) == SIG_SUCCESS
        conn.request(SIG_RUN, "MATCH (n) RETURN count(n) AS c", {}, {})
        conn.send(SIG_PULL, {"n": -1})
        sig, body = conn.recv()
        assert sig == SIG_RECORD
        assert body[:2] == b"\x91\x04", f"expected the record [4], got {body!r}"


def test_garbage_before_logon_then_reset_cannot_run_queries(auth_server):
    with RawBolt(*auth_server) as conn:
        conn.hello()
        conn.send_raw(b"\xb1\xff\xc0")  # undecodable message in the Authentication state
        try:
            conn.recv()
        except (EOFError, ConnectionError):
            pass
        assert not _counts_after_reset_ok(conn)


def test_failed_hello_then_reset_cannot_run_queries(auth_server):
    with RawBolt(*auth_server) as conn:
        conn.send_raw(b"\xb1\x01\xc0")  # HELLO whose extra is not a dict
        try:
            conn.recv()
        except (EOFError, ConnectionError):
            pass
        assert not _counts_after_reset_ok(conn)


def test_run_before_logon_is_not_executed(auth_server):
    with RawBolt(*auth_server) as conn:
        conn.hello()
        sig = conn.request(SIG_RUN, "RETURN 1", {}, {})
        assert sig != SIG_SUCCESS


def test_correct_credentials_still_work(auth_server):
    with RawBolt(*auth_server) as conn:
        assert conn.hello() == SIG_SUCCESS
        assert conn.logon(USER, PASSWORD) == SIG_SUCCESS
        assert conn.request(SIG_RUN, "RETURN 1 AS one", {}, {}) == SIG_SUCCESS
        assert conn.request(SIG_RESET) == SIG_SUCCESS
        assert conn.request(SIG_BEGIN, {}) == SIG_SUCCESS


def test_begin_after_failed_logon_is_refused(auth_server):
    with RawBolt(*auth_server) as conn:
        conn.hello()
        conn.logon(USER, "wrong")
        try:
            conn.request(SIG_RESET)
            sig = conn.request(SIG_BEGIN, {})
        except (EOFError, ConnectionError):
            sig = SIG_FAILURE
        assert sig != SIG_SUCCESS


# --- message size and nesting before / after authentication -------------------


@pytest.fixture
def open_server(tmp_path, bolt_binary_path):
    """Server without `--auth`, with a small session cap so a leak is visible."""
    if not bolt_binary_path.exists():
        pytest.skip("bolt-server binary not built")
    from tests.conftest import _build_bolt_fixture_graph, _spawn_bolt_server, _teardown_bolt_server

    graph = tmp_path / "open.kgl"
    _build_bolt_fixture_graph(graph)
    proc, url = _spawn_bolt_server(graph, extra_args=["--max-sessions", "3"])
    yield _hostport(url)
    _teardown_bolt_server(proc)


def _still_serving(addr) -> bool:
    with RawBolt(*addr) as conn:
        return conn.hello() == SIG_SUCCESS


def test_oversized_message_before_logon_is_refused_and_closed(auth_server):
    with RawBolt(*auth_server) as conn:
        sig = conn.request(SIG_HELLO, {"user_agent": "x" * (200 * 1024)})
        assert sig == SIG_FAILURE
        assert conn.is_closed()
    assert _still_serving(auth_server)


def test_large_message_after_logon_is_accepted(auth_server):
    with RawBolt(*auth_server) as conn:
        conn.hello()
        assert conn.logon(USER, PASSWORD) == SIG_SUCCESS
        assert conn.request(SIG_RUN, "RETURN size($s) AS n", {"s": "x" * (200 * 1024)}, {}) == SIG_SUCCESS


@pytest.mark.parametrize("authenticated", [False, True])
def test_deeply_nested_message_is_refused_without_aborting_the_server(auth_server, authenticated):
    deep = nested_lists(50_000)
    with RawBolt(*auth_server) as conn:
        if authenticated:
            conn.hello()
            assert conn.logon(USER, PASSWORD) == SIG_SUCCESS
            # RUN [query, {"x": <deep>}, {}]
            message = b"\xb3\x10" + pack("RETURN $x AS x") + b"\xa1" + pack("x") + deep + b"\xa0"
        else:
            message = b"\xb1\x01\xa1" + pack("user_agent") + deep
        conn.send_raw(message)
        try:
            sig, _ = conn.recv()
        except (EOFError, ConnectionError):
            sig = None
        assert sig in (SIG_FAILURE, None)
    assert _still_serving(auth_server), "the server process died on a deeply nested message"


def test_moderate_nesting_is_accepted(auth_server):
    with RawBolt(*auth_server) as conn:
        conn.hello()
        conn.logon(USER, PASSWORD)
        message = b"\xb3\x10" + pack("RETURN 1 AS one") + b"\xa1" + pack("x") + nested_lists(100) + b"\xa0"
        conn.send_raw(message)
        sig, _ = conn.recv()
        assert sig == SIG_SUCCESS


# --- session lifecycle ----------------------------------------------------------


def test_disconnect_mid_response_releases_the_session(open_server):
    big = "UNWIND range(1, 300000) AS i RETURN i, 'xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx' AS s"
    for _ in range(6):  # more attempts than --max-sessions 3
        conn = RawBolt(*open_server)
        conn.hello()
        conn.logon("u", "p")
        conn.request(SIG_RUN, big, {}, {})
        conn.send(SIG_PULL, {"n": -1})
        # Reset the socket (RST) while the server is mid-stream.
        conn.sock.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER, struct.pack("ii", 1, 0))
        conn.close()
    time.sleep(1.0)
    assert _still_serving(open_server), "leaked sessions exhausted --max-sessions"


def test_paging_with_pull_keeps_the_session_alive(tmp_path, bolt_binary_path):
    """Only RUN refreshed the idle timer in `boltr` 0.2.0, so a client paging
    through a result was reaped while it was actively pulling."""
    if not bolt_binary_path.exists():
        pytest.skip("bolt-server binary not built")
    from tests.conftest import _build_bolt_fixture_graph, _spawn_bolt_server, _teardown_bolt_server

    graph = tmp_path / "idle.kgl"
    _build_bolt_fixture_graph(graph)
    proc, url = _spawn_bolt_server(graph, extra_args=["--idle-timeout", "2"])
    try:
        with RawBolt(*_hostport(url)) as conn:
            conn.hello()
            conn.logon("u", "p")
            assert conn.request(SIG_RUN, "UNWIND range(1, 10) AS i RETURN i", {}, {}) == SIG_SUCCESS
            for _ in range(8):  # ~5.6 s of paging against a 2 s timeout
                time.sleep(0.7)
                conn.send(SIG_PULL, {"n": 1})
                assert conn.recv()[0] == SIG_RECORD
                assert conn.recv()[0] == SIG_SUCCESS
    finally:
        _teardown_bolt_server(proc)
