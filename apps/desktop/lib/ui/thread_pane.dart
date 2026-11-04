import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../state/store.dart';
import '../theme.dart';
import 'channel_view.dart';
import 'items.dart';
import 'widgets.dart';

/// The right pane when a thread is open: a History entry, a side chat (or
/// one about to start), a post's thread. Its conversation with a composer.
class ThreadPane extends StatelessWidget {
  const ThreadPane({super.key, required this.store});

  final TernStore store;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final session = store.thread;
    final t = store.threadTranscript;
    final items = t?.items ?? const [];
    final branch = store.branch;
    final replying = store.replying;
    // What a new thread starts from: a message's text, or a post.
    final quote = branch?.quote ?? replying?.text;
    final title = branch != null
        ? 'New side chat'
        : replying != null
        ? 'Reply to ${replying.author}'
        : store.sessions.where((s) => s.id == session).firstOrNull?.title ??
              'Thread';
    final waiting = store.waitingInThread();
    return Container(
      color: c.background,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          WindowDragArea(
            child: Container(
              height: titleBarInset + 12,
              padding: const EdgeInsets.fromLTRB(16, 0, 8, 0),
              child: Row(
                children: [
                  Icon(
                    quote != null ? Icons.call_split : Icons.forum_outlined,
                    size: 15,
                    color: c.muted,
                  ),
                  const SizedBox(width: 8),
                  Expanded(
                    child: Text(
                      title,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: const TextStyle(
                        fontSize: 14,
                        fontWeight: FontWeight.w600,
                      ),
                    ),
                  ),
                  QuietIconButton(
                    icon: Icons.close,
                    tooltip: 'Close',
                    onPressed: store.closeThread,
                  ),
                ],
              ),
            ),
          ),
          const Hairline(),
          if (waiting > 0) WaitingInHome(store: store, count: waiting),
          Expanded(
            child: quote != null
                ? Padding(
                    padding: const EdgeInsets.all(16),
                    child: Align(
                      alignment: Alignment.topLeft,
                      child: Container(
                        padding: const EdgeInsets.only(left: 10),
                        decoration: BoxDecoration(
                          border: Border(
                            left: BorderSide(color: c.hairline, width: 3),
                          ),
                        ),
                        child: Text(
                          quote,
                          maxLines: 8,
                          overflow: TextOverflow.ellipsis,
                          style: TextStyle(color: c.muted, fontSize: 13),
                        ),
                      ),
                    ),
                  )
                : ListView.builder(
                    reverse: true,
                    padding: const EdgeInsets.symmetric(vertical: 14),
                    itemCount: items.length,
                    itemBuilder: (context, i) => Padding(
                      padding: const EdgeInsets.symmetric(
                        horizontal: 14,
                        vertical: 6,
                      ),
                      child: ItemView(
                        item: items[items.length - 1 - i],
                        store: store,
                        session: session,
                      ),
                    ),
                  ),
          ),
          _ThreadComposer(store: store),
        ],
      ),
    );
  }
}

class _ThreadComposer extends StatefulWidget {
  const _ThreadComposer({required this.store});

  final TernStore store;

  @override
  State<_ThreadComposer> createState() => _ThreadComposerState();
}

class _ThreadComposerState extends State<_ThreadComposer> {
  final _text = TextEditingController();

  @override
  void initState() {
    super.initState();
    _text.addListener(() => setState(() {}));
  }

  @override
  void dispose() {
    _text.dispose();
    super.dispose();
  }

  void _send() {
    final text = _text.text.trim();
    if (text.isEmpty || widget.store.link != Link.online) return;
    _text.clear();
    widget.store.sendThread(text);
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final store = widget.store;
    final running = store.threadTranscript?.activeRun != null;
    return Padding(
      padding: const EdgeInsets.fromLTRB(12, 4, 12, 14),
      child: Container(
        decoration: BoxDecoration(
          border: Border.all(color: c.hairline),
          borderRadius: BorderRadius.circular(16),
        ),
        padding: const EdgeInsets.fromLTRB(12, 2, 4, 2),
        child: Row(
          crossAxisAlignment: CrossAxisAlignment.end,
          children: [
            Expanded(
              child: Focus(
                onKeyEvent: (node, e) {
                  if (e is KeyDownEvent &&
                      e.logicalKey == LogicalKeyboardKey.enter &&
                      !HardwareKeyboard.instance.isShiftPressed) {
                    _send();
                    return KeyEventResult.handled;
                  }
                  return KeyEventResult.ignored;
                },
                child: TextField(
                  controller: _text,
                  autofocus: store.branch != null || store.replying != null,
                  minLines: 1,
                  maxLines: 6,
                  decoration: InputDecoration(
                    hintText: store.branch != null
                        ? 'Ask about this'
                        : store.replying != null
                        ? 'Reply in a thread'
                        : 'Reply in this thread',
                    hintStyle: TextStyle(color: c.muted, fontSize: 13),
                    border: InputBorder.none,
                    isDense: true,
                    contentPadding: const EdgeInsets.symmetric(vertical: 10),
                  ),
                  style: const TextStyle(fontSize: 13.5),
                ),
              ),
            ),
            running && _text.text.trim().isEmpty
                ? QuietIconButton(
                    icon: Icons.stop_rounded,
                    tooltip: 'Stop',
                    onPressed: store.stopThread,
                  )
                : QuietIconButton(
                    icon: Icons.arrow_upward_rounded,
                    tooltip: 'Send (⏎)',
                    onPressed: _text.text.trim().isEmpty ? null : _send,
                  ),
          ],
        ),
      ),
    );
  }
}
