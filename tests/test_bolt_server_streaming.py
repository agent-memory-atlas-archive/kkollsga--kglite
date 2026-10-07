"""Result streaming through the Bolt server's write coalescing.

`TCP_NODELAY` (issue #201) alone makes streaming slower, because boltr sends
and flushes every RECORD separately; `coalesce.rs` batches them. These tests
pin the wire behaviour that batching must keep: every row arrives, batches
close correctly, and errors, DISCARD and oversized rows are not held back.
The write-count contract itself is asserted by the Rust unit tests.
"""

import datetime as dt
import ipaddress

import pytest

neo4j = pytest.importorskip("neo4j")

from tests.conftest import (  # noqa: E402
    _BOLT_SKIP_REASON,
    _bolt_binary_available,
    _build_bolt_fixture_graph,
    _spawn_bolt_server,
    _teardown_bolt_server,
)

pytestmark = [pytest.mark.bolt]


def _self_signed(tmp_path):
    pytest.importorskip("cryptography")
    from cryptography import x509
    from cryptography.hazmat.primitives import hashes, serialization
    from cryptography.hazmat.primitives.asymmetric import rsa
    from cryptography.x509.oid import NameOID

    key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
    name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "localhost")])
    now = dt.datetime.now(dt.timezone.utc)
    cert = (
        x509.CertificateBuilder()
        .subject_name(name)
        .issuer_name(name)
        .public_key(key.public_key())
        .serial_number(x509.random_serial_number())
        .not_valid_before(now)
        .not_valid_after(now + dt.timedelta(days=1))
        .add_extension(
            x509.SubjectAlternativeName([x509.DNSName("localhost"), x509.IPAddress(ipaddress.ip_address("127.0.0.1"))]),
            critical=False,
        )
        .sign(key, hashes.SHA256())
    )
    cert_path, key_path = tmp_path / "cert.pem", tmp_path / "key.pem"
    cert_path.write_bytes(cert.public_bytes(serialization.Encoding.PEM))
    key_path.write_bytes(
        key.private_bytes(
            serialization.Encoding.PEM,
            serialization.PrivateFormat.PKCS8,
            serialization.NoEncryption(),
        )
    )
    return cert_path, key_path


@pytest.fixture(params=["plain", "tls"])
def stream_url(request, tmp_path):
    """A running server URL, once plain and once behind TLS."""
    if not _bolt_binary_available():
        pytest.skip(_BOLT_SKIP_REASON)
    fixture_path = tmp_path / "fixture.kgl"
    _build_bolt_fixture_graph(fixture_path)
    extra = []
    if request.param == "tls":
        cert, key = _self_signed(tmp_path)
        extra = ["--tls-cert", str(cert), "--tls-key", str(key)]
    proc, url = _spawn_bolt_server(fixture_path, extra_args=extra)
    try:
        yield url.replace("bolt://", "bolt+ssc://", 1) if extra else url
    finally:
        _teardown_bolt_server(proc)


def _driver(url, **kw):
    return neo4j.GraphDatabase.driver(url, auth=("neo4j", "password"), **kw)


def test_a_long_stream_arrives_complete_and_in_order(stream_url):
    with _driver(stream_url) as driver, driver.session() as s:
        xs = [r["x"] for r in s.run("UNWIND range(1, 20000) AS x RETURN x")]
    assert xs == list(range(1, 20001))


def test_partial_pull_batches_each_close_with_has_more(stream_url):
    # fetch_size=1000: 20 PULL{n:1000} exchanges, each ending in SUCCESS.
    with _driver(stream_url, fetch_size=1000) as driver, driver.session() as s:
        xs = [r["x"] for r in s.run("UNWIND range(1, 20000) AS x RETURN x")]
    assert xs == list(range(1, 20001))


def test_discarding_a_partly_read_stream_leaves_the_connection_usable(stream_url):
    with _driver(stream_url, fetch_size=100) as driver, driver.session() as s:
        result = s.run("UNWIND range(1, 5000) AS x RETURN x")
        assert next(iter(result))["x"] == 1
        result.consume()  # DISCARD the rest
        assert s.run("RETURN 7 AS y").single()["y"] == 7


def test_a_failing_query_reports_failure_and_the_session_recovers(stream_url):
    with _driver(stream_url) as driver, driver.session() as s:
        with pytest.raises(neo4j.exceptions.Neo4jError):
            list(s.run("UNWIND range(1, 100) AS x RETURN x / 0"))
        assert s.run("UNWIND range(1, 3) AS x RETURN x").value() == [1, 2, 3]


def test_rows_larger_than_the_flush_cap_stream_through(stream_url):
    # 40 rows of 200 KB: every row alone exceeds the 64 KiB coalescing cap
    # and each is split into several Bolt chunks.
    with _driver(stream_url) as driver, driver.session() as s:
        sizes = s.run("UNWIND range(1, 40) AS i RETURN $big AS big", big="x" * 200_000).value()
    assert len(sizes) == 40
    assert all(len(v) >= 200_000 for v in sizes)


def test_interleaved_small_exchanges_still_answer_immediately(stream_url):
    with _driver(stream_url) as driver, driver.session() as s:
        for i in range(50):
            assert s.run("RETURN $i AS i", i=i).single()["i"] == i
