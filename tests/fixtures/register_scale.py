"""Deterministic generator for a Pand-shaped register: versioned objects with
large int64 ids and int titles, a closed/open validity interval per version, a
small status category, an Int32 attribute, and one anchor node per object.

The shape mirrors the national building register the register-scale program
targets (the constants below): 12-digit object numbers
with a 4-digit prefix (~3.1e12), several versions per object, about 40 % of
versions closed. Chunks are self-contained: chunk `c` owns the objects
numbered from `c * OBJECTS_PER_CHUNK_STRIDE`, so any prefix of chunks is a
valid register and `chunk(c)` is byte-identical on every call.
"""

from __future__ import annotations

from typing import NamedTuple

import numpy as np
import pandas as pd

TYPE = "Pand"
ANCHOR_TYPE = "PandObj"
REL = "VAN"
STATUSES = ("in_use", "under_construction", "permit_issued", "demolished", "not_realised")

_PREFIX = 3_100_000_000_000  # object numbers sit at ~3.1e12 (4-digit prefix x 1e12)
_VERSION_PREFIX = 3_200_000_000_000  # version ids: a disjoint ~3.2e12 range
_OBJECT_STRIDE = 100_000_000  # objects a chunk may own; far above any test chunk
_EPOCH = pd.Timestamp("1990-01-01")
_SPAN_US = 36 * 365 * 86_400 * 10**6  # first versions start within 36 years of the epoch
_MAX_VERSIONS = 5


class Chunk(NamedTuple):
    versions: pd.DataFrame  # id, ident, valid_from, valid_to, status, bouwjaar
    anchors: pd.DataFrame  # id (= the object number)
    edges: pd.DataFrame  # id -> ident, one per version


def chunk(index: int, n_versions: int, seed: int = 20260930) -> Chunk:
    """Exactly `n_versions` versions of freshly numbered objects.

    Versions of one object are consecutive: version j is valid from its start
    until the next version's start, and the last version is open except for
    ~2 % that ended. That gives about 43 % closed versions.
    Sub-second parts are populated so the µs column path is exercised.
    """
    rng = np.random.default_rng([seed, index])
    per_object = rng.choice(np.arange(1, _MAX_VERSIONS + 1), size=n_versions, p=[0.60, 0.20, 0.10, 0.06, 0.04])
    ends = np.cumsum(per_object)
    n_objects = int(np.searchsorted(ends, n_versions, side="left")) + 1
    per_object = per_object[:n_objects].copy()
    per_object[-1] -= int(ends[n_objects - 1] - n_versions)

    obj_no = _PREFIX + index * _OBJECT_STRIDE + np.arange(n_objects, dtype="int64")
    ident = np.repeat(obj_no, per_object)
    first = np.repeat(np.cumsum(per_object) - per_object, per_object)  # row index of the object's first version
    pos = np.arange(n_versions) - first  # 0-based version number within the object
    last = pos == np.repeat(per_object - 1, per_object)

    start0 = np.repeat(rng.integers(0, _SPAN_US // 2, n_objects), per_object)
    gaps = np.where(pos == 0, 0, rng.integers(30 * 86_400 * 10**6, 5 * 365 * 86_400 * 10**6, n_versions))
    csum = np.cumsum(gaps)
    start_us = start0 + csum - csum[first]

    valid_from = (_EPOCH + pd.to_timedelta(start_us, unit="us")).astype("datetime64[us]")
    nxt = np.empty(n_versions, dtype="int64")
    nxt[:-1] = start_us[1:]
    nxt[-1] = 0
    ended = rng.random(n_versions) < 0.02
    tail_len = rng.integers(30 * 86_400 * 10**6, 5 * 365 * 86_400 * 10**6, n_versions)
    end_us = np.where(last, start_us + tail_len, nxt)
    closed = ~last | ended
    valid_to = pd.Series((_EPOCH + pd.to_timedelta(end_us, unit="us")).astype("datetime64[us]")).where(closed)

    versions = pd.DataFrame(
        {
            "id": _VERSION_PREFIX + index * _OBJECT_STRIDE * _MAX_VERSIONS + np.arange(n_versions, dtype="int64"),
            "ident": ident,
            "valid_from": valid_from,
            "valid_to": valid_to.astype("datetime64[us]"),
            "status": np.array(STATUSES)[rng.choice(len(STATUSES), size=n_versions, p=[0.70, 0.05, 0.10, 0.10, 0.05])],
            "bouwjaar": rng.integers(1600, 2027, n_versions).astype("int32"),
        }
    )
    return Chunk(
        versions,
        pd.DataFrame({"id": obj_no}),
        versions[["id", "ident"]].reset_index(drop=True),
    )
