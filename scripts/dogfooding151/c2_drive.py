# -*- coding: utf-8 -*-
"""Drive `zealous serve` through create/run/approval/interrupt/resume/
get/shutdown (spec 151 C2). Uses a mock model endpoint.

Usage: python c2_drive.py <exe> <repo> <result.json> [--env K=V ...]
"""
import json
import os
import queue
import subprocess
import sys
import threading
import time


def main():
    exe, repo, out_json = sys.argv[1:4]
    env = dict(os.environ)
    args = sys.argv[4:]
    for i, a in enumerate(args):
        if a == "--env" and i + 1 < len(args):
            k, _, v = args[i + 1].partition("=")
            env[k] = v
    proc = subprocess.Popen([exe, "serve"], cwd=repo, env=env,
                            stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                            stderr=subprocess.DEVNULL, text=True,
                            encoding="utf-8", errors="replace", bufsize=1)
    q = queue.Queue()

    def reader():
        for line in proc.stdout:
            q.put(line.strip())

    threading.Thread(target=reader, daemon=True).start()
    next_id = [1]
    notifications = []
    responses = {}

    def request(cmd, params=None, timeout=60):
        rid = next_id[0]
        next_id[0] += 1
        req = {"id": rid, "cmd": cmd, "params": params or {}}
        proc.stdin.write(json.dumps(req) + "\n")
        proc.stdin.flush()
        deadline = time.time() + timeout
        while time.time() < deadline:
            try:
                line = q.get(timeout=0.2)
            except queue.Empty:
                continue
            if not line:
                continue
            msg = json.loads(line)
            if msg.get("id") == rid:
                responses[rid] = msg
                return msg
            notifications.append(msg)
        raise TimeoutError("no response for %s" % cmd)

    def wait_notification(pred, timeout=120):
        deadline = time.time() + timeout
        while time.time() < deadline:
            try:
                line = q.get(timeout=0.2)
            except queue.Empty:
                continue
            if not line:
                continue
            msg = json.loads(line)
            notifications.append(msg)
            if pred(msg):
                return msg
        raise TimeoutError("notification not arrived")

    result = {"checks": {}, "notifications": []}
    r = request("initialize", {"version": 1})
    result["checks"]["initialize_ok"] = r.get("ok") is True
    result["checks"]["protocol_version"] = (r.get("result") or {}).get("protocol_version")
    request("subscribe_events", {"task_id": "", "after_seq": 0})
    r = request("create_task", {"title": "c2 appserver resume", "brief": "Use edit_file to replace ACCEPT with PASSED in note.txt. Then report."})
    task_id = (r.get("result") or {}).get("task_id")
    result["task_id"] = task_id
    result["checks"]["create_ok"] = bool(task_id)

    request("run_task", {"task_id": task_id})
    appr1 = wait_notification(lambda m: m.get("type") == "approval_requested")
    call1 = appr1.get("call_id")
    result["checks"]["approval1_call"] = call1
    request("interrupt_task", {"task_id": task_id})
    fin1 = wait_notification(lambda m: m.get("type") == "run_finished", timeout=60)
    result["checks"]["run1_finished"] = fin1.get("status")

    r = request("run_task", {"task_id": task_id})
    result["checks"]["resume_not_already_running"] = r.get("ok") is True
    appr2 = wait_notification(lambda m: m.get("type") == "approval_requested")
    call2 = appr2.get("call_id")
    result["checks"]["approval2_call"] = call2
    result["checks"]["approval_call_rebound"] = call2 != call1
    r = request("decide_approval", {"task_id": task_id, "call_id": call2, "decision": "approve"})
    result["checks"]["decide_ok"] = r.get("ok") is True
    fin2 = wait_notification(lambda m: m.get("type") == "run_finished", timeout=180)
    result["checks"]["run2_finished"] = fin2.get("status")

    r = request("get_task", {"task_id": task_id})
    proj = r.get("result") or {}
    result["checks"]["get_status"] = proj.get("status")

    # duplicate decide must fail with no_pending_approval
    r = request("decide_approval", {"task_id": task_id, "call_id": call2, "decision": "reject"})
    result["checks"]["stale_decide_rejected"] = (r.get("ok") is False and
                                                  (r.get("error") or {}).get("code") == "no_pending_approval")

    # seq continuity from task_updated notifications
    seqs = [m.get("event", {}).get("seq") for m in notifications
            if m.get("type") == "task_updated" and isinstance(m.get("event"), dict)]
    result["seqs"] = seqs
    result["checks"]["seq_no_dup"] = len(seqs) == len(set(seqs))
    result["checks"]["seq_contiguous"] = seqs == list(range(1, len(seqs) + 1))

    r = request("shutdown", {}, timeout=60)
    result["checks"]["shutdown_ok"] = r.get("ok") is True
    try:
        proc.wait(timeout=30)
        result["checks"]["server_exit"] = proc.returncode
    except subprocess.TimeoutExpired:
        proc.kill()
        result["checks"]["server_exit"] = "TIMEOUT"
    result["notifications"] = notifications
    with open(out_json, "w", encoding="utf-8") as f:
        json.dump(result, f, ensure_ascii=False, indent=2)
    summary = {k: v for k, v in result["checks"].items()}
    print(json.dumps(summary, ensure_ascii=False))


if __name__ == "__main__":
    main()