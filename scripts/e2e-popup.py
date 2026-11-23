#!/usr/bin/env python3
"""End-to-end check of the popup path in a real tmux (needs tmux >= 3.3, a release build).

    cargo build --release && python3 scripts/e2e-popup.py

Runs on its own tmux server (-L rsfix, no config) with a client attached to a pty, so your
sessions are untouched. Checks: pick reads the snapshot instead of the live pane, snapshots
are deleted, a pane cleared AFTER the keypress still copies the screen as it was, scrollback
is reachable, and an Alt+click opens the popup with the clicked word selected (a second click
inside the popup grows the selection one level).
"""
import os, pty, subprocess, sys, time, fcntl, termios, struct, glob, shutil, tempfile
EXE=os.path.abspath("target/release/regionsel")
RT=tempfile.mkdtemp(prefix="regionsel-e2e-"); SP=RT
XDG=os.path.join(RT,"xdg"); os.makedirs(XDG+"/regionsel")
env=dict(os.environ, XDG_RUNTIME_DIR=RT, XDG_CONFIG_HOME=XDG, TERM="tmux-256color")
env.pop("REGIONSEL_CONFIG",None)
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
# ---- Test E: scrollback. 150 lines scroll off; `gg` must reach the OLDEST captured line
tm("delete-buffer")
tm("send-keys","-t","fx","clear; seq 1 150","Enter"); time.sleep(0.8)
oldest=tm("capture-pane","-p","-S","-2000","-t","fx").stdout.split("\n")[0].rstrip()
hist=int(tm("display-message","-p","-t","fx","#{history_size}").stdout)
subprocess.Popen(T+["run-shell","-b",f"{EXE} popup {pane}"],env=env)
time.sleep(1.2)
os.write(fd,b"gg"); time.sleep(0.3); os.write(fd,b"\r"); time.sleep(1.2); drain()
buf=tm("show-buffer").stdout.rstrip()
results.append((f"E scrollback: gg copies the oldest history line ({oldest!r}, history={hist})", hist>50 and buf==oldest))
# ---- Test F/G: Alt+click opens the popup with the word selected; a click in the popup grows it
tm("set","-g","mouse","on")
tm("bind-key","-n","M-MouseDown1Pane","run-shell","-b",f"{EXE} popup '#{{pane_id}}' --click '#{{mouse_x}},#{{mouse_y}}'")
tm("send-keys","-t","fx","clear; echo alpha beta gamma","Enter"); time.sleep(0.8)
rows=[l for l in tm("capture-pane","-p","-t","fx").stdout.split("\n")]
row=[i for i,l in enumerate(rows) if l.startswith("alpha beta gamma")][0]
col=rows[row].index("beta")+1              # a cell inside "beta"
def sgr(btn,x,y,down): return f"\x1b[<{btn};{x+1};{y+1}{'M' if down else 'm'}".encode()
tm("delete-buffer")
os.write(fd,sgr(8,col,row,True)+sgr(8,col,row,False)); time.sleep(1.5)   # Alt+click (button 0 + alt bit 8)
os.write(fd,b"\r"); time.sleep(1.2); drain()
results.append(("F Alt+click opens the popup with the clicked word selected", tm("show-buffer").stdout.strip()=="beta"))
tm("delete-buffer")
os.write(fd,sgr(8,col,row,True)+sgr(8,col,row,False)); time.sleep(1.5)
os.write(fd,sgr(0,col,row,True)+sgr(0,col,row,False)); time.sleep(0.5)   # plain click inside the popup
os.write(fd,b"\r"); time.sleep(1.2); drain()
results.append(("G second click inside the popup grows word -> line", tm("show-buffer").stdout.strip()=="alpha beta gamma"))
# ---- Test I/J: a user config changes the keys inside the real popup, and a typo warns
tm("unbind-key","-n","M-MouseDown1Pane")
tm("send-keys","-t","fx","clear; echo config-test-line","Enter"); time.sleep(0.8)
open(XDG+"/regionsel/config","w").write("unbind q\nbind x cancel\nbind y bogus-action\n")
tm("delete-buffer"); tm("kill-session","-t","ignore-errors")
subprocess.Popen(T+["run-shell","-b",f"{EXE} popup {pane}"],env=env)
time.sleep(1.2)
os.write(fd,b"q"); time.sleep(0.3)               # unbound: must NOT close the popup
os.write(fd,b"k"); time.sleep(0.2); os.write(fd,b"\r"); time.sleep(1.2); drain()   # still alive: copies a line
results.append(("I unbound q no longer cancels (popup still running, Enter copied)", "config-test-line" in tm("show-buffer").stdout))
msgs=tm("show-messages").stdout
results.append(("J a bad config line is reported in tmux messages with its line number", "config line 3: unknown action 'bogus-action'" in msgs))
tm("delete-buffer")
subprocess.Popen(T+["run-shell","-b",f"{EXE} popup {pane}"],env=env)
time.sleep(1.2)
os.write(fd,b"x"); time.sleep(0.8)               # remapped to cancel: popup closes
os.write(fd,b"\r"); time.sleep(0.8); drain()     # Enter now lands in the shell, not the picker
results.append(("I bind x cancel closes the popup without copying", tm("show-buffer").stdout=="" ))
os.remove(XDG+"/regionsel/config")

# ---- Test H: the viewport. A 120x40 snapshot shown in a 60x10 screen must scroll, not clip.
# Own tmux server, no client attached: with a client attached tmux would size the window to it.
V=["/usr/bin/tmux","-L","rsvp","-f","/dev/null"]
def vm(*a): return subprocess.run(V+list(a),env=env,capture_output=True,text=True)
vm("kill-server")
snap2=f"{RT}/big.ansi"
open(snap2,"w").write("\n".join(f"ROW {r:03d} " + "x"*100 + "|END" for r in range(40)) + "\n")
vm("new-session","-d","-s","vp","-x","60","-y","10",f"{EXE} pick --snapshot {snap2} --size 120x40 --cursor 5,39")
time.sleep(1.0)
def screen(): return vm("capture-pane","-p","-t","vp").stdout
def keys(*k): vm("send-keys","-t","vp",*k); time.sleep(0.4)
sc=screen()
results.append(("H precondition: the screen really is 10 rows", len(sc.rstrip("\n").split("\n"))==10))
results.append(("H starts bottom-aligned: rows 30..39 shown, 29 and 0 not", "ROW 039" in sc and "ROW 030" in sc and "ROW 029" not in sc and "ROW 000" not in sc))
results.append(("H status tag shows cursor row / total rows", "40/40" in sc))
keys("g","g"); sc=screen()
results.append(("H gg scrolls to the top", "ROW 000" in sc and "ROW 009" in sc and "ROW 010" not in sc and " 1/40" in sc))
keys("$"); sc=screen()
results.append(("H $ scrolls horizontally to the line end", "|END" in sc))
keys("C-d"); sc=screen()
results.append(("H Ctrl-d moves half a page down (row 6 of 40)", " 6/40" in sc))
keys("G"); keys("C-u"); sc=screen()
results.append(("H Ctrl-u from the bottom moves half a page up (row 35)", "35/40" in sc))
vm("kill-server")
tm("kill-server")
for n,ok in results: print(("PASS " if ok else "FAIL ")+n)
shutil.rmtree(RT,ignore_errors=True)
sys.exit(0 if all(ok for _,ok in results) else 1)
