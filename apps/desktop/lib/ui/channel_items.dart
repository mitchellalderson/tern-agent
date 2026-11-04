import 'package:flutter/material.dart';
import 'package:flutter_markdown_plus/flutter_markdown_plus.dart';
import 'package:tern_client/tern_client.dart';

import '../state/store.dart';
import '../state/transcript.dart';
import '../theme.dart';
import 'widgets.dart';

/// Where Home's stream was cut into a new session: a thin line, with the
/// summary that was carried over a click away (docs/channels-design.md §2.6).
class CutDivider extends StatefulWidget {
  const CutDivider({super.key, required this.item});

  final CutItem item;

  /// Why the cut happened, in words.
  static String reason(String? r) => switch (r) {
    'idle' => 'after a quiet spell',
    'day' => 'new day',
    'new_topic' => 'new topic',
    'migrated' => 'earlier conversation',
    _ => '',
  };

  @override
  State<CutDivider> createState() => _CutDividerState();
}

class _CutDividerState extends State<CutDivider> {
  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final cut = widget.item.cut;
    final carry = cut.carry;
    final why = CutDivider.reason(cut.reason);
    final label = [
      'New session',
      if (why.isNotEmpty) why,
      if (carry != null) 'summary carried over',
    ].join(' · ');
    final line = Expanded(child: Container(height: 1, color: c.hairline));
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Padding(
          padding: const EdgeInsets.symmetric(vertical: 6),
          child: Row(
            children: [
              line,
              Padding(
                padding: const EdgeInsets.symmetric(horizontal: 10),
                child: InkWell(
                  borderRadius: BorderRadius.circular(6),
                  onTap: carry == null
                      ? null
                      : () => setState(
                          () => widget.item.expanded = !widget.item.expanded,
                        ),
                  child: Padding(
                    padding: const EdgeInsets.symmetric(
                      horizontal: 6,
                      vertical: 2,
                    ),
                    child: Row(
                      mainAxisSize: MainAxisSize.min,
                      children: [
                        Text(
                          label,
                          style: TextStyle(fontSize: 11.5, color: c.muted),
                        ),
                        if (carry != null) ...[
                          const SizedBox(width: 2),
                          Icon(
                            widget.item.expanded
                                ? Icons.expand_less
                                : Icons.expand_more,
                            size: 14,
                            color: c.muted,
                          ),
                        ],
                      ],
                    ),
                  ),
                ),
              ),
              line,
            ],
          ),
        ),
        if (carry != null && widget.item.expanded)
          Container(
            margin: const EdgeInsets.only(top: 4),
            padding: const EdgeInsets.all(12),
            decoration: BoxDecoration(
              color: c.card,
              border: Border.all(color: c.hairline),
              borderRadius: BorderRadius.circular(10),
            ),
            child: SelectableText(
              carry,
              style: TextStyle(fontSize: 12.5, color: c.muted, height: 1.4),
            ),
          ),
      ],
    );
  }
}

/// A post from elsewhere in a stream or a timeline: who said it, when, and
/// what. Its thread opens in the right pane.
class PostCard extends StatelessWidget {
  const PostCard({super.key, required this.post, required this.store});

  final Post post;
  final TernStore store;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final session = post.sessionId;
    final tag = switch (post.authorKind) {
      'routine' => (Icons.schedule, 'routine'),
      'subagent' => (Icons.subdirectory_arrow_right, 'agent'),
      _ => null,
    };
    return Container(
      decoration: BoxDecoration(
        color: c.card,
        border: Border.all(color: c.hairline),
        borderRadius: BorderRadius.circular(12),
      ),
      padding: const EdgeInsets.fromLTRB(14, 10, 14, 12),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              AgentAvatar(name: post.author, size: 20),
              const SizedBox(width: 8),
              Expanded(
                child: Row(
                  children: [
                    Flexible(
                      child: Text(
                        post.author,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: const TextStyle(
                          fontSize: 13,
                          fontWeight: FontWeight.w600,
                        ),
                      ),
                    ),
                    if (tag case (final icon, final words)) ...[
                      const SizedBox(width: 6),
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
                  ],
                ),
              ),
              if (session != null)
                TextButton(
                  onPressed: () => store.openThread(session),
                  style: TextButton.styleFrom(
                    visualDensity: VisualDensity.compact,
                    padding: const EdgeInsets.symmetric(horizontal: 8),
                  ),
                  child: Text(
                    post.replies > 0
                        ? '${post.replies} repl${post.replies == 1 ? 'y' : 'ies'}'
                        : 'Open',
                    style: TextStyle(fontSize: 12, color: c.accent),
                  ),
                ),
            ],
          ),
          const SizedBox(height: 6),
          MarkdownBody(
            data: post.text,
            selectable: true,
            styleSheet: MarkdownStyleSheet.fromTheme(Theme.of(context))
                .copyWith(p: Theme.of(context).textTheme.bodyMedium),
          ),
        ],
      ),
    );
  }
}

/// A side chat that branched off Home's stream.
class SideChatCard extends StatelessWidget {
  const SideChatCard({super.key, required this.chat, required this.store});

  final StreamSideChat chat;
  final TernStore store;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final title = (chat.title ?? 'Side chat')
        .split('\n')
        .where((l) => !l.startsWith('>') && l.trim().isNotEmpty)
        .firstOrNull;
    return Align(
      alignment: Alignment.centerLeft,
      child: InkWell(
        borderRadius: BorderRadius.circular(10),
        onTap: () => store.openThread(chat.sessionId),
        child: Container(
          padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
          decoration: BoxDecoration(
            border: Border.all(color: c.hairline),
            borderRadius: BorderRadius.circular(10),
          ),
          child: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              Icon(Icons.call_split, size: 14, color: c.muted),
              const SizedBox(width: 8),
              Flexible(
                child: Text(
                  'Side chat · ${title ?? 'untitled'}',
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: TextStyle(fontSize: 12.5, color: c.muted),
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

/// Wraps a message in Home with a "Branch into side chat" action that
/// shows on hover.
class Branchable extends StatefulWidget {
  const Branchable({
    super.key,
    required this.text,
    required this.messageId,
    required this.store,
    required this.child,
    this.alignEnd = false,
  });

  final String text;
  final int? messageId;
  final TernStore store;
  final Widget child;
  final bool alignEnd;

  @override
  State<Branchable> createState() => _BranchableState();
}

class _BranchableState extends State<Branchable> {
  var _hover = false;

  @override
  Widget build(BuildContext context) => MouseRegion(
    onEnter: (_) => setState(() => _hover = true),
    onExit: (_) => setState(() => _hover = false),
    child: Column(
      crossAxisAlignment: widget.alignEnd
          ? CrossAxisAlignment.end
          : CrossAxisAlignment.start,
      children: [
        widget.child,
        SizedBox(
          height: 22,
          child: _hover
              ? Tooltip(
                  message: 'Branch into side chat',
                  child: InkWell(
                    borderRadius: BorderRadius.circular(6),
                    onTap: () => widget.store.branchFrom(
                      widget.text,
                      messageId: widget.messageId,
                    ),
                    child: Padding(
                      padding: const EdgeInsets.all(4),
                      child: Icon(
                        Icons.call_split,
                        size: 14,
                        color: context.colors.muted,
                      ),
                    ),
                  ),
                )
              : null,
        ),
      ],
    ),
  );
}
