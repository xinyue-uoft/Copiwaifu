#!/usr/bin/env python3
# Drives a real interactive `claude` in /tmp/cc-permtest via a PTY.
# usage: driver.py <mode> <instruction>
import pexpect, sys, time, os

mode = sys.argv[1]
instruction = sys.argv[2]
logpath = "/tmp/cc-permtest/claude_%s.log" % mode

TRUST_PAT = r"trust this folder"
READY_PAT = r"for shortcuts"
PERM_PAT  = r"(?i)(do you want to proceed|allow this|1\. Yes|❯ 1)"

env = dict(os.environ)
env.pop("CLAUDE_CONFIG_DIR", None)  # use the REAL ~/.claude (auth)

logf = open(logpath, "w")
child = pexpect.spawn(
    "/opt/homebrew/bin/claude",
    cwd="/tmp/cc-permtest", env=env,
    timeout=60, dimensions=(50, 200),
    encoding="utf-8", codec_errors="replace",
)
child.logfile = logf

# 1. Either a trust prompt OR the main UI is the first thing to appear.
try:
    i = child.expect([TRUST_PAT, READY_PAT, pexpect.TIMEOUT], timeout=25)
    if i == 0:
        time.sleep(0.6); child.send("\r")          # accept highlighted "Yes, I trust"
        print("driver: accepted trust prompt")
        child.expect([READY_PAT, pexpect.TIMEOUT], timeout=25)
    elif i == 1:
        print("driver: main UI ready (already trusted)")
    else:
        print("driver: WARNING neither trust nor ready matched")
except Exception as e:
    print("driver: startup note:", repr(e))

# 2. settle so the input box is fully interactive
time.sleep(3.0)

# 3. send the instruction that forces a single gated Bash call
child.send(instruction)
time.sleep(0.6)
child.send("\r")
print("driver: sent instruction")

# 4. watch for a permission prompt; either way wait for resolution
try:
    j = child.expect([PERM_PAT, pexpect.TIMEOUT], timeout=22)
    print("driver: permission-prompt visible" if j == 0 else "driver: no permission prompt seen")
except Exception as e:
    print("driver: wait note:", repr(e))
time.sleep(6)

# 5. drain remaining output into the log
try:
    child.read_nonblocking(size=200000, timeout=2)
except Exception:
    pass

# 6. exit cleanly
try:
    child.sendcontrol('c'); time.sleep(0.3)
    child.sendcontrol('c'); time.sleep(0.3)
    child.send("/exit\r"); time.sleep(0.8)
except Exception:
    pass
try:
    child.close(force=True)
except Exception:
    pass
logf.flush(); logf.close()
print("driver done: mode=%s" % mode)
