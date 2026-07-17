#!/usr/bin/env python3
"""A deterministic mini IMAP server for quarb-imap's tests.

Serves the three-message release-plan thread in INBOX plus an
Archive folder, plaintext, on the port given as argv[1]. Speaks
exactly the subset curl uses: LOGIN, LIST, SELECT, UID SEARCH,
UID FETCH n BODY[HEADER]/BODY[TEXT], LOGOUT.
"""
import socket, sys, threading

M = {}
def msg(uid, frm, subj, mid, irt, date, body):
    h = f"From: {frm}\r\nTo: crew@ship.dev\r\nSubject: {subj}\r\nMessage-ID: <{mid}>\r\n"
    if irt: h += f"In-Reply-To: <{irt}>\r\n"
    h += f"Date: {date}\r\n"
    return (h + "\r\n", body + "\r\n")

M["INBOX"] = {
  1: msg(1, "ada@ship.dev", "Release plan", "plan-1@ship.dev", None,
         "Tue, 7 Jul 2026 09:00:00 +0200", "Proposal: ship v0.2 on Friday."),
  2: msg(2, "bo@ship.dev", "Re: Release plan", "re-1@ship.dev", "plan-1@ship.dev",
         "Tue, 7 Jul 2026 10:30:00 +0200", "Friday works. Docs are ready."),
  4: msg(4, "cy@ship.dev", "Re: Release plan", "re-2@ship.dev", "plan-1@ship.dev",
         "Tue, 7 Jul 2026 11:45:00 +0200", "Auth tests still red, need a day."),
}
M["Archive"] = {
  7: msg(7, "ada@ship.dev", "Old notes", "old-1@ship.dev", None,
         "Mon, 2 Feb 2026 08:00:00 +0100", "Archived thoughts."),
}

def handle(c):
    f = c.makefile("rwb")
    def send(s): f.write((s + "\r\n").encode()); f.flush()
    send("* OK mini ready")
    selected = None
    while True:
        line = f.readline()
        if not line: break
        parts = line.decode().strip().split(" ", 2)
        if len(parts) < 2: continue
        tag, cmd = parts[0], parts[1].upper()
        rest = parts[2] if len(parts) > 2 else ""
        if cmd == "CAPABILITY":
            send("* CAPABILITY IMAP4rev1 LITERAL+"); send(f"{tag} OK done")
        elif cmd == "LOGIN":
            send(f"{tag} OK logged in")
        elif cmd == "LIST":
            for name in M: send(f'* LIST (\\HasNoChildren) "/" {name}')
            send(f"{tag} OK done")
        elif cmd in ("SELECT", "EXAMINE"):
            selected = rest.strip().strip('"')
            box = M.get(selected, {})
            send(f"* {len(box)} EXISTS")
            send("* OK [UIDVALIDITY 1] ok")
            send(f"* OK [UIDNEXT {max(box, default=0)+1}] ok")
            send("* FLAGS (\\Seen \\Answered \\Flagged \\Deleted \\Draft)")
            send(f"{tag} OK [READ-WRITE] done")
        elif cmd == "UID" and rest.upper().startswith("SEARCH"):
            uids = " ".join(str(u) for u in sorted(M.get(selected, {})))
            send(f"* SEARCH {uids}"); send(f"{tag} OK done")
        elif cmd == "UID" and rest.upper().startswith("FETCH"):
            args = rest.split(" ", 2)
            spec, what = args[1], args[2].upper()
            box = M.get(selected, {})
            uids = sorted(box) if (":" in spec or spec == "1:*") else \
                   [u for u in (int(x) for x in spec.split(",")) if u in box]
            if ":" in spec and spec != "1:*":
                lo, hi = spec.split(":"); lo = int(lo)
                hi = max(box, default=0) if hi == "*" else int(hi)
                uids = [u for u in sorted(box) if lo <= u <= hi]
            for i, uid in enumerate(uids, 1):
                head, body = box[uid]
                if "BODY.PEEK[]" in what or "BODY[]" in what or "RFC822" in what:
                    payload = (head + body).encode()
                    f.write((f"* {i} FETCH (UID {uid} FLAGS () "
                             f"INTERNALDATE \"07-Jul-2026 12:00:00 +0000\" "
                             f"BODY[] {{{len(payload)}}}\r\n").encode())
                    f.write(payload); f.write(b")\r\n")
                elif "HEADER" in what:
                    payload = head.encode()
                    f.write(f"* {i} FETCH (UID {uid} BODY[HEADER] {{{len(payload)}}}\r\n".encode())
                    f.write(payload); f.write(b")\r\n")
                elif "TEXT" in what:
                    payload = body.encode()
                    f.write(f"* {i} FETCH (UID {uid} BODY[TEXT] {{{len(payload)}}}\r\n".encode())
                    f.write(payload); f.write(b")\r\n")
                else:
                    f.write(f"* {i} FETCH (UID {uid} FLAGS () RFC822.SIZE {len((head+body).encode())})\r\n".encode())
            f.flush()
            send(f"{tag} OK done")
        elif cmd == "LOGOUT":
            send("* BYE"); send(f"{tag} OK done"); break
        else:
            send(f"{tag} OK ignored")
    c.close()

srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
srv.bind(("127.0.0.1", int(sys.argv[1])))
srv.listen(8)
print("ready", flush=True)
while True:
    c, _ = srv.accept()
    threading.Thread(target=handle, args=(c,), daemon=True).start()
