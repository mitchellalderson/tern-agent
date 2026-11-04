#!/usr/bin/env python3
"""Fake Telegram Bot API for tests (stdlib only). Prints its port.

Bot API (token must be TEST:TOKEN):
    getUpdates, sendMessage, editMessageText, answerCallbackQuery, sendChatAction,
    createForumTopic
Control endpoints for tests:
    POST /control/message   {"chat_id", "user_id", "text", "forum"?, "topics"?, "thread"?}  a user
                            sends a message (in a forum group's topic with "forum": true; in a
                            private chat with topics, Bot API 9.3, with "topics": true).
                            createForumTopic works only in chats seen as one of those.
    POST /control/callback  {"chat_id", "user_id", "message_id", "data"}  a user taps a button
    GET  /control/messages  everything the bot sent, with current text and buttons
"""

import json
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

TOKEN = "TEST:TOKEN"
lock = threading.Condition()
updates = []
messages = {}  # message_id -> {"message_id", "chat_id", "text", "buttons", "edits"}
next_update = [1]
next_message = [100]
next_topic = [500]
topic_chats = set()  # chats where createForumTopic works


def add_update(u):
    with lock:
        u["update_id"] = next_update[0]
        next_update[0] += 1
        updates.append(u)
        lock.notify_all()


def buttons_of(markup):
    if not markup:
        return []
    return [b for row in markup.get("inline_keyboard", []) for b in row]


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def reply(self, obj, status=200):
        data = json.dumps(obj).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def body(self):
        n = int(self.headers.get("Content-Length", 0))
        return json.loads(self.rfile.read(n) or b"{}")

    def do_GET(self):
        if self.path == "/control/messages":
            with lock:
                self.reply(sorted(messages.values(), key=lambda m: m["message_id"]))
        else:
            self.reply({"ok": False}, 404)

    def do_POST(self):
        b = self.body()
        if self.path == "/control/message":
            if b.get("forum") or b.get("topics"):
                with lock:
                    topic_chats.add(b["chat_id"])
            msg = {
                "message_id": 1,
                "chat": {"id": b["chat_id"], "type": "supergroup" if b.get("forum") else "private",
                         "is_forum": bool(b.get("forum"))},
                "from": {"id": b["user_id"]}, "text": b["text"]}
            if b.get("thread"):
                msg["message_thread_id"] = b["thread"]
                if b.get("topics"):
                    msg["is_topic_message"] = True
            add_update({"message": msg})
            return self.reply({"ok": True})
        if self.path == "/control/callback":
            with lock:
                msg = messages.get(b["message_id"], {})
            add_update({"callback_query": {
                "id": "cb%d" % b["message_id"], "from": {"id": b["user_id"]}, "data": b["data"],
                "message": {"message_id": b["message_id"], "chat": {"id": b["chat_id"]},
                            "text": msg.get("text", "")}}})
            return self.reply({"ok": True})

        prefix = "/bot" + TOKEN + "/"
        if not self.path.startswith(prefix):
            return self.reply({"ok": False, "error_code": 401, "description": "Unauthorized"}, 401)
        method = self.path[len(prefix):]
        if method == "getUpdates":
            offset, timeout = b.get("offset", 0), min(b.get("timeout", 0), 5)
            with lock:
                # Like Telegram: an offset confirms (and drops) earlier updates.
                updates[:] = [u for u in updates if u["update_id"] >= offset]
                lock.wait_for(lambda: bool(updates), timeout=timeout)
                result = list(updates)
            return self.reply({"ok": True, "result": result})
        if method == "sendMessage":
            with lock:
                mid = next_message[0]
                next_message[0] += 1
                messages[mid] = {"message_id": mid, "chat_id": b["chat_id"], "text": b["text"],
                                 "thread": b.get("message_thread_id"),
                                 "buttons": buttons_of(b.get("reply_markup")), "edits": 0}
            return self.reply({"ok": True, "result": {"message_id": mid, "chat": {"id": b["chat_id"]}}})
        if method == "editMessageText":
            with lock:
                m = messages.get(b["message_id"])
                if m is None:
                    return self.reply({"ok": False, "error_code": 400, "description": "message not found"}, 400)
                if m["text"] == b["text"]:
                    return self.reply({"ok": False, "error_code": 400,
                                       "description": "Bad Request: message is not modified"}, 400)
                m["text"] = b["text"]
                m["buttons"] = buttons_of(b.get("reply_markup"))
                m["edits"] += 1
            return self.reply({"ok": True, "result": True})
        if method == "createForumTopic":
            with lock:
                if b["chat_id"] not in topic_chats:
                    return self.reply({"ok": False, "error_code": 400,
                                       "description": "Bad Request: the chat is not a forum"}, 400)
                tid = next_topic[0]
                next_topic[0] += 1
            return self.reply({"ok": True, "result": {"message_thread_id": tid, "name": b["name"]}})
        if method in ("answerCallbackQuery", "sendChatAction"):
            return self.reply({"ok": True, "result": True})
        return self.reply({"ok": False, "error_code": 404, "description": "unknown method " + method}, 404)


server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
print(server.server_address[1], flush=True)
server.serve_forever()
