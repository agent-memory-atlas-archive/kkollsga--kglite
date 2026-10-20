import ctypes, ctypes.util, os, sys

def phys_footprint_mb():
    lib = ctypes.CDLL(ctypes.util.find_library("proc"))
    buf = (ctypes.c_uint64 * 64)()
    rc = lib.proc_pid_rusage(os.getpid(), 2, ctypes.byref(buf))  # RUSAGE_INFO_V2
    assert rc == 0, rc
    return buf[9] / 1e6  # uuid(2 words) + 7 fields -> ri_phys_footprint
