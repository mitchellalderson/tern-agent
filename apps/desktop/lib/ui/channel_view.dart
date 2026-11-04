import 'package:flutter/material.dart';
import 'package:flutter_markdown_plus/flutter_markdown_plus.dart';
import 'package:tern_client/tern_client.dart';

import '../state/store.dart';
import '../theme.dart';
import 'conversation.dart';
import 'widgets.dart';

/// A topic channel's timeline (docs/channels-design.md §5.2): its posts,
/// who is working, and a composer that posts to the channel.
class ChannelView extends StatelessWidget {
  const ChannelView({
    super.key,
    required this.store,
    required this.panelOpen,
    required this.onTogglePanel,
  });

  final TernStore store;
  final bool panelOpen;
  final VoidCallback onTogglePanel;

  /// Shown in the main timeline: the main thread, routines, and the roots
  /// of side threads (their replies live in the thread).
  static bool inTimeline(Post p, String? mainSession) =>
      p.replyTo == null ||
      p.sessionId == null ||
      p.sessionId == mainSession ||
      p.authorKind == 'routine';

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final ch = store.openChannel;
    if (ch == null) return const SizedBox.shrink();
    final main = ch.mainSessionId;
    final shown = (store.posts[ch.id] ?? const <Post>[])
        .where((p) => inTimeline(p, main))
        .toList();
    // One live line per member at work here (channels-design §5.2).
    final lines = store.liveIn(ch.id);
    final waiting = store.waitingFrom(ch.id);
    final extra = lines.length;
    return Column(
      children: [
        _ChannelHeader(
          store: store,
          channel: ch,
          panelOpen: panelOpen,
          onTogglePanel: onTogglePanel,
        ),
        const Hairline(),
        if (waiting > 0) WaitingInHome(store: store, count: waiting),
        Expanded(
          child: shown.isEmpty && lines.isEmpty
              ? _EmptyChannel(channel: ch)
              : ListView.builder(
                  reverse: true,
                  padding: const EdgeInsets.symmetric(vertical: 20),
                  itemCount: shown.length + extra,
                  itemBuilder: (context, i) => Center(
                    child: ConstrainedBox(
                      constraints: const BoxConstraints(maxWidth: 760),
                      child: Padding(
                        padding: const EdgeInsets.symmetric(
                          horizontal: 24,
                          vertical: 6,
                        ),
                        child: i < extra
                            ? MemberLine(line: lines[extra - 1 - i])
                            : TimelinePost(
                                post: shown[shown.length - 1 - (i - extra)],
                                store: store,
                              ),
                      ),
                    ),
                  ),
                ),
        ),
        if (store.error case final e?)
          Padding(
            padding: const EdgeInsets.fromLTRB(24, 0, 24, 8),
            child: Text(e, style: TextStyle(color: c.error, fontSize: 12.5)),
          ),
        Composer(store: store),
      ],
    );
  }
}

/// "research-1 · Running http_fetch": a member working in the channel.
class MemberLine extends StatelessWidget {
  const MemberLine({super.key, required this.line});

  final ({String runId, String author, String doing}) line;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    return Row(
      children: [
        AgentAvatar(name: line.author, size: 18),
        const SizedBox(width: 8),
        SizedBox(
          width: 10,
          height: 10,
          child: CircularProgressIndicator(strokeWidth: 1.5, color: c.muted),
        ),
        const SizedBox(width: 8),
        Flexible(
          child: Text(
            '${line.author} · ${line.doing}',
            maxLines: 1,
            overflow: TextOverflow.ellipsis,
            style: TextStyle(fontSize: 12.5, color: c.muted),
          ),
        ),
      ],
    );
  }
}

/// "Waiting for you in Home": approvals and secret requests are answered
/// there, whichever channel asked.
class WaitingInHome extends StatelessWidget {
  const WaitingInHome({super.key, required this.store, required this.count});

  final TernStore store;
  final int count;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    return Material(
      color: c.warn.withValues(alpha: 0.1),
      child: InkWell(
        onTap: () => store.openHome(),
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: 20, vertical: 8),
          child: Row(
            children: [
              Icon(Icons.pan_tool_alt_outlined, size: 15, color: c.warn),
              const SizedBox(width: 8),
              Expanded(
                child: Text(
                  count == 1
                      ? 'Waiting for you in Home'
                      : '$count things waiting for you in Home',
                  style: const TextStyle(fontSize: 12.5),
                ),
              ),
              Text(
                'Go to Home',
                style: TextStyle(fontSize: 12.5, color: c.accent),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

class _ChannelHeader extends StatelessWidget {
  const _ChannelHeader({
    required this.store,
    required this.channel,
    required this.panelOpen,
    required this.onTogglePanel,
  });

  final TernStore store;
  final Channel channel;
  final bool panelOpen;
  final VoidCallback onTogglePanel;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    // Who has posted lately, newest first.
    final members = <String>[];
    for (final p in (store.posts[channel.id] ?? const <Post>[]).reversed) {
      if (!p.mine && !members.contains(p.author)) members.add(p.author);
      if (members.length == 4) break;
    }
    final main = channel.mainSessionId;
    return WindowDragArea(
      child: Container(
        height: titleBarInset + 12,
        padding: const EdgeInsets.fromLTRB(20, 0, 10, 0),
        alignment: Alignment.center,
        child: Row(
          children: [
            Text(
              '#',
              style: TextStyle(
                fontSize: 17,
                color: c.muted,
                fontWeight: FontWeight.w600,
              ),
            ),
            const SizedBox(width: 6),
            Expanded(
              child: Row(
                children: [
                  Flexible(
                    child: Text(
                      channel.title,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: const TextStyle(
                        fontSize: 15,
                        fontWeight: FontWeight.w600,
                      ),
                    ),
                  ),
                  if (channel.instructions case final i?) ...[
                    const SizedBox(width: 6),
                    Tooltip(
                      message: i,
                      child: Icon(Icons.rule, size: 15, color: c.muted),
                    ),
                  ],
                ],
              ),
            ),
            for (final m in members)
              Padding(
                padding: const EdgeInsets.only(right: 3),
                child: Tooltip(
                  message: m,
                  child: AgentAvatar(name: m, size: 20),
                ),
              ),
            const SizedBox(width: 6),
            if (main != null)
              if (store.usage[main]
                  case Usage(
                    :final contextTokens?,
                    :final contextWindow?,
                    :final compactAt,
                  )
                  when contextWindow > 0)
                ContextMeter(
                  used: contextTokens,
                  window: contextWindow,
                  compactAt: compactAt,
                ),
            QuietIconButton(
              icon: Icons.tune,
              tooltip: 'Channel settings',
              onPressed: () => showChannelSettings(context, store, channel),
            ),
            QuietIconButton(
              icon: panelOpen
                  ? Icons.view_sidebar
                  : Icons.view_sidebar_outlined,
              tooltip: panelOpen ? 'Hide activity' : 'Show activity',
              onPressed: onTogglePanel,
            ),
          ],
        ),
      ),
    );
  }
}

class _EmptyChannel extends StatelessWidget {
  const _EmptyChannel({required this.channel});

  final Channel channel;

  @override
  Widget build(BuildContext context) => Center(
    child: Column(
      mainAxisSize: MainAxisSize.min,
      children: [
        Text(
          '#${channel.slug}',
          style: Theme.of(context).textTheme.titleMedium
              ?.copyWith(fontWeight: FontWeight.w600),
        ),
        const SizedBox(height: 6),
        Text(
          'Ask tern something here, or point a routine at this channel.',
          style: Theme.of(context).textTheme.bodySmall,
        ),
      ],
    ),
  );
}

/// One post in a channel's timeline.
class TimelinePost extends StatelessWidget {
  const TimelinePost({super.key, required this.post, required this.store});

  final Post post;
  final TernStore store;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final theme = Theme.of(context);
    final tag = switch (post.authorKind) {
      'routine' => (Icons.schedule, 'routine'),
      'subagent' => (Icons.subdirectory_arrow_right, 'under a parent agent'),
      _ => null,
    };
    final session = post.sessionId;
    return Row(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        post.mine
            ? CircleAvatar(
                radius: 13,
                backgroundColor: c.userBubble,
                child: Icon(Icons.person, size: 15, color: c.muted),
              )
            : AgentAvatar(name: post.author, size: 26),
        const SizedBox(width: 10),
        Expanded(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Row(
                children: [
                  Text(
                    post.author,
                    style: const TextStyle(
                      fontSize: 13,
                      fontWeight: FontWeight.w600,
                    ),
                  ),
                  if (post.tainted) ...[
                    const SizedBox(width: 5),
                    const TaintMark(),
                  ],
                  if (tag case (final icon, final words)) ...[
                    const SizedBox(width: 5),
                    Tooltip(
                      message: words,
                      child: Icon(icon, size: 13, color: c.muted),
                    ),
                  ],
                  const SizedBox(width: 8),
                  Text(
                    relativeTime(post.createdAt),
                    style: TextStyle(fontSize: 11.5, color: c.muted),
                  ),
                  if (post.needsYou) ...[
                    const SizedBox(width: 8),
                    const NeedsYouTag(),
                  ],
                ],
              ),
              const SizedBox(height: 3),
              MarkdownBody(
                data: post.text,
                selectable: true,
                styleSheet: MarkdownStyleSheet.fromTheme(theme)
                    .copyWith(p: theme.textTheme.bodyMedium),
              ),
              Row(
                children: [
                  if (post.needsYou)
                    _Action(label: 'Done', onTap: () => store.resolvePost(post))
                  else if (post.kind == 'needs_you')
                    const _Action(label: 'Done ✓', onTap: null),
                  _Action(
                    label: 'Reply in thread',
                    onTap: () => store.replyTo(post),
                  ),
                  if (_sideThread() case final side? when post.replies > 0)
                    _Action(
                      label:
                          '${post.replies} repl${post.replies == 1 ? 'y' : 'ies'}',
                      onTap: () => store.openThread(side),
                    ),
                  // Its run, tool steps and all.
                  if (session != null && !post.mine)
                    _Action(
                      label: 'Open thread',
                      onTap: () => store.openThread(session),
                    ),
                ],
              ),
            ],
          ),
        ),
      ],
    );
  }
}

extension on TimelinePost {
  /// The side thread replying to this post, from the replies loaded.
  String? _sideThread() {
    final main = store.openChannel?.mainSessionId;
    return (store.posts[post.channelId] ?? const <Post>[])
        .where((p) => p.replyTo == post.id && p.sessionId != main)
        .map((p) => p.sessionId)
        .nonNulls
        .firstOrNull;
  }
}

/// Marks a post that waits on the user.
class NeedsYouTag extends StatelessWidget {
  const NeedsYouTag({super.key, this.where});

  /// For a pointer: the channel it came from.
  final String? where;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 1),
      decoration: BoxDecoration(
        color: c.warn.withValues(alpha: 0.15),
        borderRadius: BorderRadius.circular(6),
      ),
      child: Text(
        where == null ? 'needs you' : 'needs you · $where',
        style: TextStyle(fontSize: 11, color: c.warn),
      ),
    );
  }
}

class _Action extends StatelessWidget {
  const _Action({required this.label, required this.onTap});

  final String label;
  final VoidCallback? onTap;

  @override
  Widget build(BuildContext context) => InkWell(
    borderRadius: BorderRadius.circular(6),
    onTap: onTap,
    child: Padding(
      padding: const EdgeInsets.fromLTRB(0, 4, 12, 2),
      child: Text(
        label,
        style: TextStyle(fontSize: 12, color: context.colors.muted),
      ),
    ),
  );
}

/// `[a-z0-9][a-z0-9-]{0,31}`, as the daemon wants it.
String slugFor(String title) {
  final s = title
      .toLowerCase()
      .replaceAll(RegExp(r'[^a-z0-9]+'), '-')
      .replaceAll(RegExp(r'^-+|-+$'), '');
  return s.length > 32 ? s.substring(0, 32).replaceAll(RegExp(r'-+$'), '') : s;
}

/// The + beside CHANNELS.
Future<void> showNewChannel(BuildContext context, TernStore store) =>
    showDialog<void>(
      context: context,
      builder: (_) => ChannelDialog(store: store),
    );

Future<void> showChannelSettings(
  BuildContext context,
  TernStore store,
  Channel channel,
) => showDialog<void>(
  context: context,
  builder: (_) => ChannelDialog(store: store, editing: channel),
);

/// Create a topic channel, or change one's title and instructions.
class ChannelDialog extends StatefulWidget {
  const ChannelDialog({super.key, required this.store, this.editing});

  final TernStore store;
  final Channel? editing;

  @override
  State<ChannelDialog> createState() => _ChannelDialogState();
}

class _ChannelDialogState extends State<ChannelDialog> {
  late final _title = TextEditingController(text: widget.editing?.title);
  late final _instructions = TextEditingController(
    text: widget.editing?.instructions,
  );
  String? _error;
  var _busy = false;

  @override
  void initState() {
    super.initState();
    _title.addListener(() => setState(() {}));
  }

  @override
  void dispose() {
    _title.dispose();
    _instructions.dispose();
    super.dispose();
  }

  String get _slug => slugFor(_title.text);

  Future<void> _save() async {
    setState(() {
      _busy = true;
      _error = null;
    });
    final editing = widget.editing;
    final err = editing == null
        ? await widget.store.createChannel(
            slug: _slug,
            title: _title.text.trim(),
            instructions: _instructions.text.trim().isEmpty
                ? null
                : _instructions.text.trim(),
          )
        : await widget.store.updateChannel(
            editing.id,
            title: _title.text.trim(),
            instructions: _instructions.text.trim(),
          );
    if (!mounted) return;
    if (err == null) {
      Navigator.of(context).pop();
    } else {
      setState(() {
        _busy = false;
        _error = err;
      });
    }
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final editing = widget.editing;
    final labels = Theme.of(context).textTheme.labelSmall;
    final ok = _slug.isNotEmpty && !_busy;
    return Dialog(
      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(14)),
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 460),
        child: Padding(
          padding: const EdgeInsets.fromLTRB(22, 20, 22, 16),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              Text(
                editing == null ? 'New channel' : '#${editing.slug}',
                style: const TextStyle(
                  fontSize: 17,
                  fontWeight: FontWeight.w600,
                ),
              ),
              const SizedBox(height: 4),
              Text(
                'A channel is a topic: its own timeline, the same memory. '
                'Routines can post their results here.',
                style: Theme.of(context).textTheme.bodySmall,
              ),
              const SizedBox(height: 16),
              Text('NAME', style: labels),
              const SizedBox(height: 6),
              TextField(
                controller: _title,
                autofocus: editing == null,
                decoration: InputDecoration(
                  hintText: 'Finances',
                  isDense: true,
                  border: OutlineInputBorder(
                    borderRadius: BorderRadius.circular(8),
                  ),
                  helperText: editing == null && _slug.isNotEmpty
                      ? '#$_slug'
                      : null,
                ),
              ),
              const SizedBox(height: 14),
              Text('INSTRUCTIONS', style: labels),
              const SizedBox(height: 6),
              TextField(
                controller: _instructions,
                minLines: 2,
                maxLines: 5,
                decoration: InputDecoration(
                  hintText: 'Amounts in EUR. Never move money.',
                  isDense: true,
                  border: OutlineInputBorder(
                    borderRadius: BorderRadius.circular(8),
                  ),
                  helperText: 'Added to what tern is told for work here.',
                ),
              ),
              if (_error case final e?) ...[
                const SizedBox(height: 10),
                Text(e, style: TextStyle(color: c.error, fontSize: 12.5)),
              ],
              const SizedBox(height: 16),
              Row(
                children: [
                  if (editing != null)
                    TextButton(
                      onPressed: _busy
                          ? null
                          : () async {
                              Navigator.of(context).pop();
                              await widget.store.archiveChannel(editing.id);
                            },
                      child: Text('Archive', style: TextStyle(color: c.error)),
                    ),
                  const Spacer(),
                  TextButton(
                    onPressed: () => Navigator.of(context).pop(),
                    child: const Text('Cancel'),
                  ),
                  const SizedBox(width: 8),
                  FilledButton(
                    onPressed: ok ? _save : null,
                    child: Text(editing == null ? 'Create' : 'Save'),
                  ),
                ],
              ),
            ],
          ),
        ),
      ),
    );
  }
}
