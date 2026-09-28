"""The no-argument clock functions: ``datetime()``, ``date()`` and ``time()``
read the clock in UTC — the zone every stored datetime, validity instant and
``auto_timestamp`` stamp is naive in — and ``localdatetime()`` /
``localtime()`` read the process's local zone, as their names say.

The probe runs in a child process pinned to a zone with a non-whole-hour
offset (Asia/Kathmandu, UTC+05:45), so it cannot pass by the machine being in
UTC. Red proof: ``datetime()`` and ``time()`` returned the local wall clock,
5 h 45 min off UTC.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys

PROBE = """
import datetime as dt, json, kglite
g = kglite.KnowledgeGraph()
row = g.cypher(
    "RETURN datetime() AS dt, localdatetime() AS ldt, date() AS d, time() AS t, localtime() AS lt"
).to_list()[0]
utc = dt.datetime.now(dt.timezone.utc).replace(tzinfo=None)
local = dt.datetime.now()
def seconds_of_day(text):
    h, m, s = map(int, text.split(":"))
    return h * 3600 + m * 60 + s
def clock_gap(a, b):
    gap = abs(a - b) % 86400
    return min(gap, 86400 - gap)
now_utc = utc.hour * 3600 + utc.minute * 60 + utc.second
now_local = local.hour * 3600 + local.minute * 60 + local.second
print(json.dumps({
    "offset": (local - utc).total_seconds(),
    "datetime": abs((row["dt"] - utc).total_seconds()),
    "localdatetime": abs((row["ldt"] - local).total_seconds()),
    "date": row["d"].isoformat() == utc.date().isoformat(),
    "time": clock_gap(seconds_of_day(row["t"]), now_utc),
    "localtime": clock_gap(seconds_of_day(row["lt"]), now_local),
}))
"""


def test_the_clock_functions_read_utc_and_the_local_ones_the_local_zone() -> None:
    env = dict(os.environ, TZ="Asia/Kathmandu")
    out = subprocess.run([sys.executable, "-c", PROBE], env=env, capture_output=True, text=True, check=True)
    probe = json.loads(out.stdout)
    # The zone is in force in the child: local is UTC + 5:45.
    assert abs(probe["offset"] - 5.75 * 3600) < 60, probe
    assert probe["datetime"] < 60, probe
    assert probe["localdatetime"] < 60, probe
    assert probe["date"] is True, probe
    assert probe["time"] < 60, probe
    assert probe["localtime"] < 60, probe
