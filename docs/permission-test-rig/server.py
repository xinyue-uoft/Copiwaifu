#!/usr/bin/env python3
# Controllable PermissionRequest endpoint for the fail-open test.
# usage: server.py <mode> <port> [hang_seconds]
# modes: allow | deny | empty200 | drop | hang
import socket, sys, time, json, os, struct

mode = sys.argv[1]
port = int(sys.argv[2])
hang_s = float(sys.argv[3]) if len(sys.argv) > 3 else 15.0

logpath = "/tmp/cc-permtest/server_%s.log" % mode
logf = open(logpath, "a")
def log(*a):
    print(time.strftime("%H:%M:%S"), *a, file=logf, flush=True)

srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
srv.bind(("127.0.0.1", port))
srv.listen(8)
log("LISTENING mode=%s port=%d" % (mode, port))
print("LISTENING", mode, port, flush=True)

def read_request(conn):
    conn.settimeout(4.0)
    data = b""
    try:
        while b"\r\n\r\n" not in data:
            chunk = conn.recv(4096)
            if not chunk:
                break
            data += chunk
        # best-effort body drain by Content-Length
        head = data.split(b"\r\n\r\n", 1)
        if len(head) == 2:
            cl = 0
            for line in head[0].split(b"\r\n"):
                if line.lower().startswith(b"content-length:"):
                    try: cl = int(line.split(b":",1)[1].strip())
                    except: cl = 0
            body = head[1]
            while len(body) < cl:
                chunk = conn.recv(4096)
                if not chunk: break
                body += chunk
    except Exception as e:
        log("read err", repr(e))
    return data[:300]

while True:
    try:
        conn, addr = srv.accept()
    except Exception as e:
        log("accept err", repr(e)); continue
    head = read_request(conn)
    log("HIT from", addr, "head=", head)
    if mode == "allow":
        body = json.dumps({"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"}}}).encode()
        conn.sendall(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: %d\r\nConnection: close\r\n\r\n" % len(body) + body)
        log("-> sent ALLOW"); conn.close()
    elif mode == "deny":
        body = json.dumps({"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"deny","message":"denied by fail-open test"}}}).encode()
        conn.sendall(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: %d\r\nConnection: close\r\n\r\n" % len(body) + body)
        log("-> sent DENY"); conn.close()
    elif mode == "empty200":
        conn.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
        log("-> sent EMPTY 200"); conn.close()
    elif mode == "drop":
        # abortive close (RST), no HTTP response at all — mirrors Node res.destroy()
        try:
            conn.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER, struct.pack("ii", 1, 0))
        except Exception as e:
            log("linger err", repr(e))
        conn.close()
        log("-> DROPPED connection (RST, no response)")
    elif mode == "hang":
        log("-> HANGING %.1fs" % hang_s)
        time.sleep(hang_s)
        conn.close()
        log("-> hang closed")
    else:
        conn.close()
