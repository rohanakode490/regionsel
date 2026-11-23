#!/usr/bin/env python3
"""End-to-end check of the popup path in a real tmux (needs tmux >= 3.3, a release build).

    cargo build --release && python3 scripts/e2e-popup.py

Runs on its own tmux server (-L rsfix, no config) with a client attached to a pty, so your
sessions are untouched. Checks: pick reads the snapshot instead of the live pane, snapshots
are deleted, and a pane cleared AFTER the keypress still copies the screen as it was.
"""
import os, pty, subprocess, sys, time, fcntl, termios, struct, glob, shutil, tempfile
EXE=os.path.abspath("target/release/regionsel")
RT=tempfile.mkdtemp(prefix="regionsel-e2e-"); SP=RT
env=dict(os.environ, XDG_RUNTIME_DIR=RT, TERM="tmux-256color")
env.pop("TMUX",None)
T=["/usr/bin/tmux","-L","rsfix","-f","/dev/null"]
def tm(*a,**k): return subprocess.run(T+list(a),env=env,capture_output=True,text=True,**k)
tm("kill-server")
tm("new-session","-d","-s","fx","-x","120","-y","40","env PS1='$ ' bash --norc")
time.sleep(0.5)
tm("send-keys","-t","fx","echo LIVE-MARKER-ONE","Enter"); time.sleep(0.4)
# attach a real client on a pty
pid,fd=pty.fork()
if pid==0:
    os.execvpe(T[0],T+["attach","-t","fx"],env)
fcntl.ioctl(fd,termios.TIOCSWINSZ,struct.pack("HHHH",40,120,0,0)); time.sleep(0.8)
def drain():
    import select
    while select.select([fd],[],[],0.2)[0]:
        try: os.read(fd,65536)
        except OSError: break
drain()
results=[]
# ---- Test A: pick reads a hand-made snapshot that differs from the live pane
snap=f"{RT}/handmade.ansi"; open(snap,"w").write("SNAPSHOT-MARKER-ROW0\n"+"\n"*39)
cmd=f"{EXE} pick --snapshot {snap} --size 120x40 --cursor 0,0"
subprocess.Popen(T+["display-popup","-B","-E","-x","0","-y","0","-w","120","-h","40",cmd],env=env)
time.sleep(1.0); os.write(fd,b"\r"); time.sleep(1.0); drain()
buf=tm("show-buffer").stdout
results.append(("A pick uses the snapshot, not the live pane", "SNAPSHOT-MARKER-ROW0" in buf and "LIVE-MARKER" not in buf))
results.append(("A snapshot file deleted by pick", not os.path.exists(snap)))
tm("delete-buffer")
# ---- Test B: full path `regionsel popup %0` (capture, write, popup, cleanup)
pane=tm("display-message","-p","-t","fx","#{pane_id}").stdout.strip()
subprocess.Popen(T+["run-shell","-b",f"{EXE} popup {pane}"],env=env)
time.sleep(1.2)
mid=glob.glob(f"{RT}/regionsel-*.ansi")
os.write(fd,b"k"); time.sleep(0.2); os.write(fd,b"\r"); time.sleep(1.2); drain()
buf=tm("show-buffer").stdout
results.append(("B popup copies a line of the screen as captured", "LIVE-MARKER-ONE" in buf))
results.append(("B no snapshot left behind", glob.glob(f"{RT}/regionsel-*.ansi")==[]))
if mid:
    pass
# ---- Test C: cancel leaves nothing behind
subprocess.Popen(T+["run-shell","-b",f"{EXE} popup {pane}"],env=env)
time.sleep(1.2); os.write(fd,b"q"); time.sleep(1.0); drain()
results.append(("C cancel with q leaves no snapshot", glob.glob(f"{RT}/regionsel-*.ansi")==[]))
# ---- Test D: the pane is cleared AFTER the popup opens; the copy must still be the old screen
tm("delete-buffer")
subprocess.Popen(T+["run-shell","-b",f"{EXE} popup {pane}"],env=env)
time.sleep(1.2)
tm("send-keys","-t","fx","clear","Enter"); time.sleep(0.6)   # underlying pane repaints while popup is open
live=tm("capture-pane","-p","-t","fx").stdout
os.write(fd,b"k"); time.sleep(0.2); os.write(fd,b"\r"); time.sleep(1.2); drain()
buf=tm("show-buffer").stdout
results.append(("D live pane no longer has the marker (precondition)", "LIVE-MARKER-ONE" not in live))
results.append(("D copy still returns the screen as of the keypress", "LIVE-MARKER-ONE" in buf))
tm("kill-server")
for n,ok in results: print(("PASS " if ok else "FAIL ")+n)
shutil.rmtree(RT,ignore_errors=True)
sys.exit(0 if all(ok for _,ok in results) else 1)
